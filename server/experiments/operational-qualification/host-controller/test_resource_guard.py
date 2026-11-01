"""Offline only: no guard, subprocess, systemd, socket or namespace executes."""

import json
import os
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

import resource_guard as r

UNIT = "waddle-resource-1000-" + "a" * 32 + ".service"
PATH = (
    f"/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service/app.slice/{UNIT}"
)


def properties():
    return dict(
        r.READBACK,
        Id=UNIT,
        LoadState="loaded",
        ActiveState="active",
        SubState="running",
        InvocationID="1" * 32,
        MainPID="42",
        ExecMainPID="42",
        ControlGroup=PATH,
        Result="success",
        ExecMainCode="0",
        ExecMainStatus="0",
        FragmentPath=r.transient_directory() + "/" + UNIT,
    )


def encoded(props):
    return "".join(k + "=" + v + "\n" for k, v in props.items()).encode()


def proc_stat(start=123, state=b"T"):
    return b"42 (guard) " + b" ".join([state] + [b"0"] * 18 + [str(start).encode()])


class PipeReportingTests(unittest.TestCase):
    def fixture(self):
        out_read, out_write = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)
        err_read, err_write = os.pipe2(os.O_CLOEXEC | os.O_NONBLOCK)

        def close_owned(fd, identity):
            try:
                if r.file_identity(fd) == identity:
                    os.close(fd)
            except OSError:
                pass

        for fd in (out_read, out_write, err_read, err_write):
            self.addCleanup(close_owned, fd, r.file_identity(fd))
        return r.PipeReports((out_write, err_write)), (
            out_read,
            out_write,
            err_read,
            err_write,
        )

    def fill(self, fd):
        for _ in range(257):
            try:
                os.write(fd, b"x" * 4096)
            except BlockingIOError:
                return
        self.fail("fixture pipe exceeds bounded capacity")

    def invoke(self, reports, behavior=None):
        with (
            patch.object(r, "PipeReports", return_value=reports),
            patch.object(
                r, "main", side_effect=behavior, return_value={"ok": True}
            ) as main,
        ):
            code = r.entrypoint()
        return code, main

    def test_successful_bounded_pipe_report(self):
        reports, (out_read, _, err_read, _) = self.fixture()
        code, main = self.invoke(reports)
        self.assertEqual(code, 0)
        self.assertEqual(json.loads(os.read(out_read, r.MAX_REPORT)), {"ok": True})
        with self.assertRaises(BlockingIOError):
            os.read(err_read, 1)
        main.assert_called_once()

    def test_regular_file_stdout_or_stderr_rejected_before_manager_contact(self):
        for channel in (0, 1):
            self.check_regular_file(channel)

    def test_two_regular_file_captures_reject_without_any_write(self):
        with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
            reports = r.PipeReports((out.fileno(), err.fileno()))
            with (
                patch.object(r.os, "write") as write,
                patch.object(r.Host, "command") as command,
            ):
                code, main = self.invoke(reports)
            self.assertEqual(code, r.REPORTING_EXIT)
            write.assert_not_called()
            command.assert_not_called()
            main.assert_not_called()
            self.assertEqual(os.fstat(out.fileno()).st_size, 0)
            self.assertEqual(os.fstat(err.fileno()).st_size, 0)

    def test_two_closed_readers_exit_without_fallback_write(self):
        reports, (out_read, _, err_read, _) = self.fixture()
        os.close(out_read)
        os.close(err_read)
        with patch.object(r.os, "write") as write:
            code, main = self.invoke(reports)
        self.assertEqual(code, r.REPORTING_EXIT)
        main.assert_not_called()
        write.assert_not_called()

    def check_regular_file(self, channel):
        reports, (out_read, out_write, err_read, err_write) = self.fixture()
        with tempfile.TemporaryFile() as file:
            reports.fds = (
                (file.fileno(), err_write)
                if channel == 0
                else (out_write, file.fileno())
            )
            with (
                patch.object(r.Host, "command") as command,
                patch.object(r.subprocess, "Popen") as popen,
            ):
                code, main = self.invoke(reports)
            self.assertEqual(code, r.REPORTING_EXIT)
            main.assert_not_called()
            command.assert_not_called()
            popen.assert_not_called()
            self.assertEqual(os.fstat(file.fileno()).st_size, 0)
            reader = err_read if channel == 0 else out_read
            self.assertEqual(
                json.loads(os.read(reader, r.MAX_REPORT)),
                {"ok": False, "check": "output-pipes-required"},
            )

    def test_closed_or_full_pipe_rejected_before_manager_contact(self):
        for condition in ("closed", "full"):
            self.check_initial_pipe_failure(condition)

    def check_initial_pipe_failure(self, condition):
        reports, (out_read, out_write, err_read, _) = self.fixture()
        if condition == "closed":
            os.close(out_read)
        else:
            self.fill(out_write)
        with (
            patch.object(r.Host, "command") as command,
            patch.object(r.subprocess, "Popen") as popen,
        ):
            code, main = self.invoke(reports)
        self.assertEqual(code, r.REPORTING_EXIT)
        main.assert_not_called()
        command.assert_not_called()
        popen.assert_not_called()
        self.assertEqual(
            json.loads(os.read(err_read, r.MAX_REPORT))["check"],
            "output-pipes-required",
        )

    def test_closed_or_full_pipe_after_validation_has_no_fallback_or_retry(self):
        for condition in ("closed", "full"):
            self.check_reporting_pipe_failure(condition)

    def check_reporting_pipe_failure(self, condition):
        reports, (out_read, out_write, err_read, _) = self.fixture()

        def run():
            if condition == "closed":
                os.close(out_read)
            else:
                self.fill(out_write)
            return {"ok": True}

        code, main = self.invoke(reports, run)
        self.assertEqual(code, r.REPORTING_EXIT)
        main.assert_called_once()
        with self.assertRaises(BlockingIOError):
            os.read(err_read, 1)

    def test_write_error_or_short_write_gets_exactly_one_attempt(self):
        for failure in (
            BrokenPipeError("private pipe detail"),
            BlockingIOError(),
            InterruptedError(),
            0,
        ):
            self.check_write_failure(failure)

    def check_write_failure(self, failure):
        reports, (out_read, _, err_read, _) = self.fixture()
        kwargs = (
            {"return_value": failure}
            if type(failure) is int
            else {"side_effect": failure}
        )
        with patch.object(r.os, "write", **kwargs) as write:
            code, main = self.invoke(reports)
        self.assertEqual(code, r.REPORTING_EXIT)
        write.assert_called_once()
        main.assert_called_once()
        for fd in (out_read, err_read):
            with self.assertRaises(BlockingIOError):
                os.read(fd, 1)

    def test_report_serialization_or_size_error_never_attempts_write(self):
        for result in (
            {"ok": True, "opaque": object()},
            {"ok": True, "large": "x" * r.MAX_REPORT},
        ):
            reports, _ = self.fixture()
            with patch.object(r.os, "write") as write:
                code, _ = self.invoke(reports, lambda result=result: result)
            self.assertEqual(code, r.REPORTING_EXIT)
            write.assert_not_called()

    def test_entrypoint_errors_emit_only_fixed_bounded_names(self):
        for failure in (
            OSError("private host path"),
            RuntimeError("secret manager detail"),
        ):
            reports, (out_read, _, _, _) = self.fixture()
            code, _ = self.invoke(reports, failure)
            self.assertEqual(code, 1)
            data = os.read(out_read, r.MAX_REPORT)
            self.assertEqual(
                json.loads(data), {"ok": False, "check": "entrypoint-failed"}
            )
            self.assertNotIn(b"secret", data)
            self.assertNotIn(b"private", data)

    def test_read_only_or_replaced_pipe_is_not_supported(self):
        reports, (out_read, _, _, err_write) = self.fixture()
        reports.fds = (out_read, err_write)
        with self.assertRaisesRegex(r.Rejected, "output-pipes-required"):
            reports.validate()
        reports, _ = self.fixture()
        reports.validate()
        fd = reports.fds[0]
        reports.pipes[fd] = (999, 999)
        with patch.object(r.os, "write") as write:
            self.assertFalse(reports.send({"ok": True}))
        write.assert_not_called()


class PolicyTests(unittest.TestCase):
    def test_saved_v262_transcript_has_explicit_fixture_identity_and_strict_fields(
        self,
    ):
        data = (
            Path(__file__).parent / "fixtures/systemd-v262-failed-readback.txt"
        ).read_bytes()
        fixture_unit = "waddle-resource-1000-00000000000000000000000000000001.service"
        props = r.parse(data)
        r.controls(props, fixture_unit)
        self.assertEqual(props["InvocationID"], "1" * 32)
        self.assertEqual(props["ExecMainPID"], "4242")
        self.assertEqual(props["Result"], "timeout")
        self.assertNotIn("CPUAccounting", r.PROPERTIES)
        self.assertNotIn("CPUAccounting", r.FIELDS)
        self.assertEqual(props["CPUQuotaPerSecUSec"], "500ms")
        self.assertEqual(props["CPUQuotaPeriodUSec"], "100ms")
        for field in r.FIELDS:
            missing = b"".join(
                line
                for line in data.splitlines(keepends=True)
                if not line.startswith((field + "=").encode())
            )
            with (
                self.subTest(field=field),
                self.assertRaisesRegex(r.Rejected, "property-fields"),
            ):
                r.parse(missing)

    def test_every_required_control_is_rejected_when_missing_or_unlimited(self):
        self.assertEqual(r.parse(encoded(properties())), properties())
        for key in r.FIELDS:
            with self.subTest(missing=key), self.assertRaises(r.Rejected):
                props = properties()
                del props[key]
                r.parse(encoded(props))
        for key in r.READBACK:
            for invalid in ("infinity", "max", "", "-1", "18446744073709551615"):
                if invalid == r.READBACK[key]:
                    continue
                with (
                    self.subTest(control=key, invalid=invalid),
                    self.assertRaises(r.Rejected),
                ):
                    r.controls(dict(properties(), **{key: invalid}), UNIT)

    def test_duplicate_malformed_and_oversized_properties(self):
        for data in (
            b"",
            b"garbage",
            b"A=B\n",
            b"\xff",
            encoded(properties()) + b"Id=x\n",
            b"x" * (r.CAP + 1),
        ):
            with self.subTest(data=data[:20]), self.assertRaises(r.Rejected):
                r.parse(data)

    def test_invocation_is_required_and_nonzero(self):
        for value in ("", "1" * 31, "z" * 32, "0" * 32):
            with self.subTest(value=value), self.assertRaises(r.Rejected):
                r.controls(dict(properties(), InvocationID=value), UNIT)

    def test_cgroup_events_covers_descendant_population_and_malformed(self):
        self.assertTrue(r.empty_events(b"populated 0\nfrozen 0\n"))
        self.assertFalse(r.empty_events(b"populated 1\nfrozen 0\n"))
        for data in (
            b"",
            b"populated max\n",
            b"populated 0\npopulated 1\n",
            b"populated\n",
            b"x" * 129,
        ):
            with self.subTest(data=data), self.assertRaises(r.Rejected):
                r.empty_events(data)

    def test_proc_stat_and_pid_format(self):
        self.assertEqual(r.process_stat(proc_stat()), (b"T", 123))
        for value in ("0", "1", "-42", "42x", "2" * 20):
            with self.assertRaises(r.Rejected):
                r.number(value)


class ConfigurationTests(unittest.TestCase):
    def test_strict_manager_path_parser_rejects_ambiguous_responses(self):
        self.assertEqual(
            r.unit_paths(b"UnitPath=/runtime/control /runtime/generator\n"),
            ("/runtime/control", "/runtime/generator"),
        )
        for data in (
            b"",
            b"UnitPath=\n",
            b"UnitPath=/a /a\n",
            b"UnitPath=relative\n",
            b"UnitPath=/a//b\n",
            b"UnitPath=/a/../b\n",
            b"UnitPath=/a/./b\n",
            b"UnitPath=/space\\x20root\n",
            b'UnitPath="/quoted root"\n',
            b"UnitPath=/one\nUnitPath=/two\n",
            b"UnitPath=/a\x00b\n",
            b"UnitPath=/a\xffb\n",
            b"UnitPath=" + b"x" * r.CAP,
            b"UnitPath=" + b" ".join(f"/{i}".encode() for i in range(65)) + b"\n",
        ):
            with self.subTest(data=data[:30]), self.assertRaises(r.Rejected):
                r.unit_paths(data)

    def test_37_and_64_bounded_unit_roots_are_supported(self):
        for count in (37, 64):
            roots = tuple(f"/root-{i}" for i in range(count))
            self.assertEqual(
                r.unit_paths(("UnitPath=" + " ".join(roots) + "\n").encode()), roots
            )

    def test_every_root_and_config_dependency_category_is_checked(self):
        with tempfile.TemporaryDirectory() as directory:
            roots = tuple(
                str(Path(directory) / name)
                for name in (
                    "user.control",
                    "runtime",
                    "generator.early",
                    "generator",
                    "generator.late",
                )
            )
            for root in roots:
                Path(root).mkdir()
            r.configuration_snapshot(roots, UNIT, lambda: None)
            required = {
                "service.d",
                UNIT,
                UNIT + ".d",
                UNIT + ".wants",
                UNIT + ".requires",
                UNIT + ".upholds",
            }
            required.update(
                "waddle-resource-1000-.service" + suffix
                for suffix in (".d", ".wants", ".requires", ".upholds")
            )
            self.assertTrue(required <= set(r.configuration_names(UNIT)))
            for root in roots:
                for name in r.configuration_names(UNIT):
                    candidate = Path(root) / name
                    candidate.mkdir()
                    with (
                        self.subTest(root=root, name=name),
                        self.assertRaisesRegex(
                            r.Rejected, "inherited-unit-configuration"
                        ),
                    ):
                        r.configuration_snapshot(roots, UNIT, lambda: None)
                    candidate.rmdir()

    def test_existing_symlinks_files_and_unresolved_roots_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "units"
            root.mkdir()
            candidate = root / "service.d"
            candidate.symlink_to(root / "missing")
            with self.assertRaisesRegex(r.Rejected, "inherited-unit-configuration"):
                r.configuration_snapshot((str(root),), UNIT, lambda: None)
            candidate.unlink()
            candidate.write_text("not a directory")
            with self.assertRaisesRegex(r.Rejected, "inherited-unit-configuration"):
                r.configuration_snapshot((str(root),), UNIT, lambda: None)
            candidate.unlink()
            unresolved = Path(directory) / "unresolved"
            unresolved.symlink_to(Path(directory) / "missing")
            with self.assertRaisesRegex(r.Rejected, "unit-path-unresolved"):
                r.configuration_snapshot((str(unresolved),), UNIT, lambda: None)
            unresolved.unlink()
            unresolved.symlink_to(unresolved)
            with self.assertRaisesRegex(r.Rejected, "unit-path-unresolved"):
                r.configuration_snapshot((str(unresolved),), UNIT, lambda: None)

    def test_readable_resolved_symlink_and_decidable_absent_roots(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "target"
            target.mkdir()
            link = Path(directory) / "link"
            link.symlink_to(target)
            missing = Path(directory) / "absent" / "nested"
            snapshot = r.configuration_snapshot(
                (str(link), str(missing)), UNIT, lambda: None
            )
            self.assertEqual(snapshot[0][1:3], ("present", str(target)))
            self.assertEqual(snapshot[1][1], "absent")

    def test_unreadable_root_is_not_treated_as_absent(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "units"
            root.mkdir()
            real_open = os.open

            def denied(path, flags, **kwargs):
                if Path(path) == root:
                    raise PermissionError()
                return real_open(path, flags, **kwargs)

            with (
                patch.object(r.os, "open", side_effect=denied),
                self.assertRaises(PermissionError),
            ):
                r.configuration_snapshot((str(root),), UNIT, lambda: None)

    def test_fresh_manager_paths_and_filesystem_drift_prevent_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            one, two = Path(directory) / "one", Path(directory) / "two"
            one.mkdir()
            two.mkdir()

            def response(path):
                return f"UnitPath={path}\n".encode()

            host = r.Host(
                {"ctl": "fixed-ctl", "run": "fixed-run", "guard": "fixed-guard"}, UNIT
            )
            host.command = Mock(side_effect=[response(one), response(two)])
            host.runtime_path = str(one)
            self.addCleanup(host.close)
            with (
                patch.object(r, "lifecycle_review_required"),
                self.assertRaisesRegex(
                    r.Rejected,
                    "manager-unit-path-drift|transient-directory-not-in-unit-path",
                ),
            ):
                host.launch()
            self.assertEqual(host.command.call_count, 2)
            self.assertTrue(
                all(
                    "--property=UnitPath" in call.args[0]
                    for call in host.command.call_args_list
                )
            )
            host = r.Host(
                {"ctl": "fixed-ctl", "run": "fixed-run", "guard": "fixed-guard"}, UNIT
            )
            count = 0

            def changed(_argv):
                nonlocal count
                count += 1
                if count == 2:
                    one.rename(one.with_name("old"))
                    one.mkdir()
                return response(one)

            host.command = Mock(side_effect=changed)
            host.runtime_path = str(one)
            self.addCleanup(host.close)
            with (
                patch.object(r, "lifecycle_review_required"),
                self.assertRaisesRegex(
                    r.Rejected,
                    "manager-unit-path-drift|transient-directory-replacement",
                ),
            ):
                host.launch()
            self.assertEqual(host.command.call_count, 2)

    def test_preflight_covers_current_paths_before_fixed_launch(self):
        with tempfile.TemporaryDirectory() as directory:
            host = r.Host(
                {"ctl": "fixed-ctl", "run": "fixed-run", "guard": "fixed-guard"}, UNIT
            )
            host.command = Mock(
                side_effect=[f"UnitPath={directory}\n".encode()] * 2 + [b""]
            )
            host.runtime_path = directory
            self.addCleanup(host.close)
            with patch.object(r, "lifecycle_review_required"):
                host.launch()
            self.assertEqual(host.command.call_count, 3)
            self.assertIn("--job-mode=fail", host.command.call_args.args[0])
            (Path(directory) / "service.d").mkdir()
            host.command.side_effect = None
            host.command.return_value = f"UnitPath={directory}\n".encode()
            host.launch_known = False  # Exercise the absence-only preflight again.
            with self.assertRaisesRegex(r.Rejected, "inherited-unit-configuration"):
                host.verify_configuration()


class FragmentTests(unittest.TestCase):
    def fixture(self, extra_root=False):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        runtime = Path(directory.name) / "transient"
        runtime.mkdir()
        roots = [str(runtime)]
        if extra_root:
            other = Path(directory.name) / "generator"
            other.mkdir()
            roots.append(str(other))
        roots = tuple(roots)
        host = r.Host({"ctl": "fixed-ctl"}, UNIT)
        host.runtime_path = str(runtime)
        self.addCleanup(host.close)
        host.configuration = (roots, r.configuration_snapshot(roots, UNIT, host.tick))
        host.pin_runtime(roots)
        host.launch_known = True
        host.dead = Mock(return_value=False)
        host.quiesced = Mock(return_value=False)
        host.command = Mock(
            return_value=("UnitPath=" + " ".join(roots) + "\n").encode()
        )
        props = dict(properties(), FragmentPath=str(runtime / UNIT))
        fragment = runtime / UNIT
        fragment.write_text(
            "# fixed manager fixture\n[Service]\nType=exec\nRestart=no\n"
        )
        host.pin_fragment(props)
        return host, props, fragment, roots

    def test_expected_manager_fragment_is_allowed_after_known_launch(self):
        host, props, _, _ = self.fixture()
        host.verify_configuration(props)
        self.assertTrue(host.fragment_present)
        self.assertEqual(host.fragment_invocation, props["InvocationID"])

    def test_fragment_disappearance_requires_confirmed_stop_dead_guard_and_quiescence(
        self,
    ):
        for missing_condition in ("stop", "stopped", "dead", "quiesced", None):
            host, props, fragment, _ = self.fixture()
            host.stop_sent = missing_condition != "stop"
            host.unit_stopped = missing_condition != "stopped"
            host.dead.return_value = missing_condition != "dead"
            host.quiesced.return_value = missing_condition != "quiesced"
            fragment.unlink()
            if missing_condition is not None:
                with (
                    self.subTest(condition=missing_condition),
                    self.assertRaisesRegex(
                        r.Rejected, "transient-fragment-disappeared"
                    ),
                ):
                    host.check_fragment(allow_missing=True)
            else:
                gone = dict(
                    props,
                    LoadState="not-found",
                    MainPID="0",
                    FragmentPath="",
                    InvocationID="",
                    Transient="no",
                )
                host.verify_configuration(gone, allow_missing=True)
                self.assertFalse(host.fragment_present)

    def test_fragment_retained_after_stop_is_valid_until_manager_gc(self):
        host, props, fragment, _ = self.fixture()
        host.stop_sent = host.unit_stopped = True
        host.dead.return_value = host.quiesced.return_value = True
        host.verify_configuration(
            dict(props, MainPID="0", ActiveState="inactive", SubState="dead"),
            allow_missing=True,
        )
        self.assertTrue(fragment.exists())
        self.assertTrue(host.fragment_present)

    def test_replacement_mutation_symlink_and_content_bounds_are_rejected(self):
        for kind in ("replace", "mutate", "symlink", "oversized", "empty"):
            host, _, fragment, _ = self.fixture()
            if kind == "replace":
                content = fragment.read_bytes()
                fragment.unlink()
                fragment.write_bytes(content)
            elif kind == "symlink":
                fragment.unlink()
                fragment.symlink_to(fragment.parent / "other")
            else:
                fragment.write_bytes(
                    {
                        "mutate": b"changed",
                        "oversized": b"x" * (r.CAP + 1),
                        "empty": b"",
                    }[kind]
                )
            with self.subTest(kind=kind), self.assertRaises((r.Rejected, OSError)):
                host.check_fragment()

    def test_spoofed_properties_and_unowned_fragment_are_rejected(self):
        host, props, _, _ = self.fixture()
        for field, value in (
            ("FragmentPath", "/elsewhere/" + UNIT),
            ("SourcePath", "/source"),
            ("DropInPaths", "/drop-in"),
            ("Transient", "no"),
            ("InvocationID", "2" * 32),
        ):
            with self.subTest(field=field), self.assertRaises(r.Rejected):
                host.verify_configuration(dict(props, **{field: value}))
        real_fstat = os.fstat

        def unowned(fd):
            info = real_fstat(fd)
            if r.stat.S_ISREG(info.st_mode):
                values = list(info)
                values[4] = os.getuid() + 1
                return os.stat_result(values)
            return info

        with (
            patch.object(r.os, "fstat", side_effect=unowned),
            self.assertRaisesRegex(r.Rejected, "transient-fragment-file"),
        ):
            host.check_fragment()

    def test_extra_configuration_and_fragment_in_another_root_are_rejected(self):
        host, props, _, roots = self.fixture(extra_root=True)
        extra = Path(roots[1]) / UNIT
        extra.write_text("injected")
        with self.assertRaisesRegex(r.Rejected, "unexpected-transient-fragment"):
            host.verify_configuration(props)
        extra.unlink()
        for suffix in (".d", ".wants", ".requires", ".upholds"):
            extra = Path(roots[0]) / (UNIT + suffix)
            extra.mkdir()
            with (
                self.subTest(suffix=suffix),
                self.assertRaisesRegex(r.Rejected, "inherited-unit-configuration"),
            ):
                host.verify_configuration(props)
            extra.rmdir()

    def test_unknown_launch_and_missing_runtime_root_never_authorize_fragment(self):
        host, props, _, roots = self.fixture()
        host.launch_known = False
        with self.assertRaisesRegex(r.Rejected, "transient-fragment-ownership"):
            host.fragment_properties(props)
        with self.assertRaisesRegex(r.Rejected, "transient-directory-not-in-unit-path"):
            host.pin_runtime((roots[0] + "-other",))

    def test_transient_parent_replacement_is_rejected_with_old_fd_retained(self):
        host, _, fragment, _ = self.fixture()
        parent = fragment.parent
        parent.rename(parent.with_name("original"))
        parent.mkdir()
        self.assertEqual(r.file_identity(host.runtimefd), host.runtime_identity)
        with self.assertRaisesRegex(r.Rejected, "transient-directory-replacement"):
            host.check_fragment()

    def test_unloaded_properties_before_original_stop_are_rejected(self):
        host, props, _, _ = self.fixture()
        gone = dict(
            props,
            LoadState="not-found",
            MainPID="0",
            FragmentPath="",
            InvocationID="",
            Transient="no",
        )
        with self.assertRaisesRegex(r.Rejected, "transient-fragment-properties"):
            host.fragment_properties(gone, allow_missing=True)


class TranscriptTransport:
    """Pure CLI transcript that exercises Host.command's real bounded pump."""

    def __init__(self, respond):
        self.respond = respond
        self.streams = {}
        self.next_fd = 900000

    def popen(self, argv, **_kwargs):
        streams = []
        for data in (self.respond(argv), b""):
            fd = self.next_fd
            self.next_fd += 1
            stream = SimpleNamespace(fileno=lambda value=fd: value, close=lambda: None)
            self.streams[fd] = bytearray(data)
            streams.append(stream)
        return SimpleNamespace(
            stdout=streams[0],
            stderr=streams[1],
            pid=999999,
            poll=lambda: 0,
            wait=lambda **_kwargs: 0,
        )

    class Selector:
        def __init__(self):
            self.items = {}

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def register(self, stream, _event):
            self.items[stream.fileno()] = SimpleNamespace(
                fd=stream.fileno(), fileobj=stream
            )

        def unregister(self, stream):
            del self.items[stream.fileno()]

        def get_map(self):
            return self.items

        def select(self, _timeout):
            return [(value, 1) for value in self.items.values()]

    def install(self, stack):
        real_read = os.read
        real_set_blocking = os.set_blocking

        def read(fd, count):
            if fd not in self.streams:
                return real_read(fd, count)
            data = bytes(self.streams[fd][:count])
            del self.streams[fd][:count]
            return data

        def blocking(fd, value):
            if fd not in self.streams:
                return real_set_blocking(fd, value)
            return None

        stack.enter_context(patch.object(r.subprocess, "Popen", side_effect=self.popen))
        stack.enter_context(patch.object(r.selectors, "DefaultSelector", self.Selector))
        stack.enter_context(patch.object(r.os, "read", side_effect=read))
        stack.enter_context(patch.object(r.os, "set_blocking", side_effect=blocking))


class FullLifecycleTests(unittest.TestCase):
    def test_known_launch_then_failure_before_fragment_or_process_pin_never_signals(
        self,
    ):
        for failure in ("transient-fragment-path", "property-fields"):
            with self.subTest(primary=failure):
                self.check_unpinned_failure(failure)

    def check_unpinned_failure(self, failure):
        with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
            runtime = Path(directory) / "transient"
            runtime.mkdir()
            host = r.Host(
                {"run": "fixed-run", "ctl": "fixed-ctl", "guard": "fixed-guard"}, UNIT
            )
            host.runtime_path = str(runtime)
            fragment = runtime / UNIT
            calls = []

            def respond(argv):
                calls.append(argv)
                if argv[0] == "fixed-run":
                    fragment.write_text("# manager fixture, ownership not yet pinned\n")
                    return b""
                if "--property=UnitPath" in argv:
                    return f"UnitPath={runtime}\n".encode()
                if failure == "property-fields":
                    transcript = (
                        Path(__file__).parent
                        / "fixtures/systemd-v262-failed-readback.txt"
                    ).read_bytes()
                    return b"".join(
                        line
                        for line in transcript.splitlines(keepends=True)
                        if not line.startswith(b"SourcePath=")
                    )
                return encoded(dict(properties(), FragmentPath="/untrusted/" + UNIT))

            TranscriptTransport(respond).install(stack)
            stack.enter_context(patch.object(r, "lifecycle_review_required"))
            send = stack.enter_context(patch.object(r.signal, "pidfd_send_signal"))
            kill = stack.enter_context(patch.object(r.os, "killpg"))
            guessed_pid = stack.enter_context(patch.object(r.os, "kill"))
            result = r.inspect(host)
            self.assertTrue(host.launch_known)
            self.assertIsNone(host.saved)
            self.assertIsNone(host.fragmentfd)
            self.assertIsNone(host.pidfd)
            self.assertEqual(
                result,
                {
                    "ok": False,
                    "userspace_quiesced": False,
                    "kernel_reclamation": "unverified",
                    "check": "cleanup-unverified",
                    "primary_failure": failure,
                    "cleanup_failure": "cleanup-ownership-unknown",
                },
            )
            send.assert_not_called()
            kill.assert_not_called()
            guessed_pid.assert_not_called()
            self.assertEqual(sum(argv[0] == "fixed-run" for argv in calls), 1)
            self.assertFalse(any("kill" in argv or "stop" in argv for argv in calls))
            self.assertTrue(fragment.exists())

    def test_realistic_37_and_64_root_lifecycles_fit_bounded_phase_budgets(self):
        for count in (37, 64):
            with self.subTest(roots=count):
                self.check_lifecycle(count)

    def check_lifecycle(self, count):
        with tempfile.TemporaryDirectory() as directory, ExitStack() as stack:
            base = Path(directory)
            runtime = base / "runtime" / "systemd" / "transient"
            runtime.mkdir(parents=True)
            roots = (str(runtime),) + tuple(
                str(
                    base
                    / (
                        "nix-profile-0123456789abcdefghijklmnop-systemd-user-runtime-unit-search-root-"
                        + str(i)
                    )
                )
                for i in range(count - 1)
            )
            for root in roots[1:]:
                Path(root).mkdir()
            response = ("UnitPath=" + " ".join(roots) + "\n").encode()
            self.assertLess(len(response), r.CAP)
            state = {"dead": False, "stopped": False}
            totals = {"preflight": 0, "inspection": 0, "cleanup": 0}

            class FixtureHost(r.Host):
                phase = "preflight"

                def command(self, argv):
                    if argv[0] == "fixed-run":
                        self.phase = "inspection"
                    return super().command(argv)

                def pin(self):
                    props = self.show()
                    self.pin_fragment(props)
                    self.saved = props
                    self.start, self.identity = 123, (44, 55)
                    self.events = 999998
                    self.pidfd = 999997
                    self.check()

                def check(self):
                    props = self.show()
                    r.controls(props, self.unit)
                    self.fragment_properties(props)
                    self.check_fragment()

                def dead(self):
                    return state["dead"]

                def quiesced(self):
                    self.quiescence_evidence = (
                        "removed-guard-dead-reclamation-unverified"
                    )
                    return state["dead"]

                def same_group(self, _props):
                    return "removed" if state["dead"] else "present"

                def cleanup(self):
                    self.phase = "cleanup"
                    super().cleanup()

                def close(self):
                    for fd in (self.fragmentfd, self.runtimefd):
                        if fd is not None:
                            os.close(fd)

            host = FixtureHost(
                {"run": "fixed-run", "ctl": "fixed-ctl", "guard": "fixed-guard"},
                UNIT,
            )
            host.runtime_path = str(runtime)
            fragment = runtime / UNIT

            def respond(argv):
                if argv[0] == "fixed-run":
                    fragment.write_text(
                        "# programmatically created manager fixture\n[Service]\nType=exec\n"
                    )
                    data = b""
                elif "--property=UnitPath" in argv:
                    data = response
                elif "stop" in argv:
                    state["stopped"] = True
                    fragment.unlink()
                    data = b""
                else:
                    props = dict(properties(), FragmentPath=str(fragment))
                    if state["dead"]:
                        props.update(
                            MainPID="0",
                            SubState="exited",
                            ExecMainCode="1",
                            ControlGroup="",
                        )
                    if state["stopped"]:
                        props.update(
                            LoadState="not-found",
                            ActiveState="inactive",
                            SubState="dead",
                            Transient="no",
                            FragmentPath="",
                            InvocationID="",
                        )
                    data = encoded(props)
                totals[host.phase] += len(data)
                return data

            TranscriptTransport(respond).install(stack)
            stack.enter_context(patch.object(r, "lifecycle_review_required"))
            stack.enter_context(
                patch.object(
                    r.signal,
                    "pidfd_send_signal",
                    side_effect=lambda *_args: state.update(dead=True),
                )
            )
            result = r.inspect(host)
            self.assertTrue(result["ok"], result)
            self.assertFalse(fragment.exists())
            self.assertEqual(result["kernel_reclamation"], "unverified")
            self.assertEqual(
                result["observed"]["transient_fragment"], "absent-after-stop"
            )
            self.assertGreater(totals["inspection"], r.CAP)
            self.assertTrue(
                all(0 < size < r.PHASE_OUTPUT_CAP for size in totals.values()),
                totals,
            )

    def test_aggregate_phase_budget_is_enforced_across_successful_calls(self):
        host = r.Host({}, UNIT)
        with ExitStack() as stack:
            TranscriptTransport(lambda _argv: b"x" * r.CAP).install(stack)
            for _ in range(r.PHASE_OUTPUT_CAP // r.CAP):
                self.assertEqual(len(host.command(["fixed"])), r.CAP)
            with self.assertRaisesRegex(r.Rejected, "command-output-cap"):
                host.command(["fixed"])


class PublicationTests(unittest.TestCase):
    def host(self, statuses):
        host = r.Host({}, UNIT)
        host.saved = properties()
        host.dead = Mock(return_value=True)
        host.group_state = Mock(return_value="present")
        host.show = Mock(side_effect=statuses)
        return host

    def test_dead_pidfd_retries_only_unchanged_identity_until_published(self):
        complete = dict(properties(), MainPID="0", SubState="exited", ExecMainCode="1")
        host = self.host([properties(), dict(properties(), MainPID="0"), complete])
        with patch.object(r.time, "sleep") as sleep:
            self.assertEqual(host.published_exit(), complete)
        self.assertEqual(host.show.call_count, 3)
        self.assertEqual(sleep.call_count, 2)

    def test_publication_drift_fails_immediately_without_retry(self):
        for field, value in (
            ("MainPID", "43"),
            ("ExecMainPID", "43"),
            ("InvocationID", "2" * 32),
            ("ControlGroup", "/different"),
        ):
            host = self.host([dict(properties(), **{field: value})])
            with patch.object(r.time, "sleep") as sleep, self.assertRaises(r.Rejected):
                host.published_exit()
            sleep.assert_not_called()
            host.show.assert_called_once()

    def test_publication_wait_has_a_deadline(self):
        host = self.host([properties()])
        host.tick = Mock(side_effect=[None, r.Rejected("deadline")])
        with (
            patch.object(r.time, "sleep"),
            self.assertRaisesRegex(r.Rejected, "deadline"),
        ):
            host.published_exit()
        host.show.assert_called_once()


class ParentPathTests(unittest.TestCase):
    def test_parent_deleted_and_recreated_with_old_fd_still_open(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = Path(directory) / "app.slice"
            parent.mkdir()
            leaf = parent / UNIT
            leaf.mkdir()
            host = r.Host({}, UNIT)
            host.saved = dict(properties(), ControlGroup=f"/app.slice/{UNIT}")
            host.parentfd = os.open(parent, os.O_RDONLY | os.O_DIRECTORY)
            host.cgfd = os.open(leaf, os.O_RDONLY | os.O_DIRECTORY)
            host.parent_identity, host.identity = (
                r.file_identity(host.parentfd),
                r.file_identity(host.cgfd),
            )
            host.dead = Mock(return_value=True)

            def reopen(path):
                return os.open(
                    Path(directory) / path.lstrip("/"), os.O_RDONLY | os.O_DIRECTORY
                )

            try:
                with patch.object(r, "open_group", side_effect=reopen):
                    self.assertEqual(host.group_state(), "present")
                    leaf.rmdir()
                    parent.rmdir()
                    parent.mkdir()
                    self.assertEqual(
                        r.file_identity(host.parentfd), host.parent_identity
                    )
                    with self.assertRaisesRegex(
                        r.Rejected, "cgroup-parent-replacement"
                    ):
                        host.group_state()
            finally:
                os.close(host.cgfd)
                os.close(host.parentfd)


class ObservationTests(unittest.TestCase):
    def fixture(self):
        host = r.Host({"guard": "/nix/store/fixed/guard"}, UNIT)
        host.saved = properties()
        host.start, host.identity = 123, (44, 55)
        host.parent_identity = (44, 55)
        host.parentfd = 70
        host.procfd, host.cgfd, host.events, host.pidfd = 20, 30, 40, 50
        host.show = Mock(return_value=properties())
        host.verify_configuration = Mock()
        host.dead = Mock(return_value=False)
        host.quiesced = Mock(return_value=False)
        files = dict(
            r.LIMITS,
            **{
                "stat": proc_stat(),
                "cgroup": ("0::" + PATH + "\n").encode(),
                "environ": b"",
                "status": f"Uid:\t{os.getuid()}\t{os.getuid()}\t{os.getuid()}\t{os.getuid()}\nNoNewPrivs:\t1\nThreads:\t1\n".encode(),
                "cgroup.type": b"domain\n",
                "cgroup.controllers": b"cpu memory pids\n",
                "cgroup.procs": b"42\n",
            },
        )

        real_stat = os.stat

        def fake_stat(path, **kwargs):
            if str(path).startswith("/proc/self/fd/"):
                return SimpleNamespace(st_mode=0o20666, st_rdev=os.makedev(1, 3))
            if path == "exe" or path == "/nix/store/fixed/guard":
                return SimpleNamespace(st_mode=0o100555, st_dev=1, st_ino=2)
            return real_stat(path, **kwargs)

        stack = ExitStack()
        self.addCleanup(stack.close)
        stack.enter_context(
            patch.object(
                r, "read_at", side_effect=lambda _fd, name, *_args: files[name]
            )
        )
        stack.enter_context(patch.object(r, "open_group", return_value=80))
        stack.enter_context(patch.object(r.os, "open", return_value=60))
        identities = stack.enter_context(
            patch.object(r, "file_identity", return_value=(44, 55))
        )
        stack.enter_context(patch.object(r.os, "close"))
        stack.enter_context(patch.object(r.os, "stat", side_effect=fake_stat))
        stack.enter_context(patch.object(r.os, "listdir", return_value=["0", "1", "2"]))
        return host, files, identities

    def test_valid_stopped_guard(self):
        host, _, _ = self.fixture()
        host.check()

    def test_pid_and_invocation_replacement_never_release(self):
        for key, value in (
            ("MainPID", "43"),
            ("ExecMainPID", "43"),
            ("InvocationID", "2" * 32),
        ):
            host, _, _ = self.fixture()
            host.show.return_value[key] = value
            with (
                patch.object(r.signal, "pidfd_send_signal") as send,
                self.assertRaises(r.Rejected),
            ):
                host.release()
            send.assert_not_called()

    def test_start_time_cgroup_inode_or_barrier_drift_never_release(self):
        for kind in (
            "start",
            "inode",
            "running",
            "controller",
            "limit",
            "environment",
            "descendant",
        ):
            with self.subTest(kind=kind):
                host, files, identities = self.fixture()
                if kind == "inode":
                    identities.return_value = (44, 66)
                else:
                    key, value = {
                        "start": ("stat", proc_stat(456)),
                        "running": ("stat", proc_stat(state=b"R")),
                        "controller": ("cgroup.controllers", b"cpu memory\n"),
                        "limit": ("memory.max", b"max\n"),
                        "environment": ("environ", b"X"),
                        "descendant": ("cgroup.procs", b"42\n43\n"),
                    }[kind]
                    files[key] = value
                with (
                    patch.object(r.signal, "pidfd_send_signal") as send,
                    self.assertRaises(r.Rejected),
                ):
                    host.release()
                send.assert_not_called()

    def test_cleanup_replacement_refuses_all_signals(self):
        host, _, _ = self.fixture()
        host.command = Mock()
        host.show.return_value["InvocationID"] = "2" * 32
        with self.assertRaises(r.Rejected):
            host.cleanup()
        host.command.assert_not_called()

    def test_removed_original_leaf_requires_dead_guard(self):
        host, _, _ = self.fixture()
        host.quiesced = r.Host.quiesced.__get__(host)
        with patch.object(r.os, "open", side_effect=FileNotFoundError()):
            with self.assertRaisesRegex(r.Rejected, "removed-while-guard-alive"):
                host.group_state()
            host.dead.return_value = True
            self.assertTrue(host.quiesced())
        self.assertEqual(
            host.quiescence_evidence, "removed-guard-dead-reclamation-unverified"
        )

    def test_removal_racing_event_read_and_leaf_reuse(self):
        host, _, _ = self.fixture()
        host.quiesced = r.Host.quiesced.__get__(host)
        host.dead.return_value = True
        with (
            patch.object(host, "group_state", side_effect=["present", "removed"]),
            patch.object(r.os, "lseek", side_effect=OSError()),
        ):
            self.assertTrue(host.quiesced())
        self.assertEqual(
            host.quiescence_evidence, "removed-guard-dead-reclamation-unverified"
        )
        host, _, _ = self.fixture()
        host.dead.return_value = True
        with (
            patch.object(
                r,
                "file_identity",
                side_effect=lambda fd: (44, 66) if fd == 60 else (44, 55),
            ),
            self.assertRaisesRegex(r.Rejected, "cgroup-replacement"),
        ):
            host.group_state()

    def test_unreadable_present_group_does_not_mean_empty(self):
        host, _, _ = self.fixture()
        host.quiesced = r.Host.quiesced.__get__(host)
        with (
            patch.object(host, "group_state", return_value="present"),
            patch.object(r.os, "lseek", side_effect=PermissionError()),
            self.assertRaisesRegex(r.Rejected, "cgroup-events-unreadable"),
        ):
            host.quiesced()

    def test_retained_parent_replacement_is_rejected(self):
        host, _, _ = self.fixture()
        host.parent_identity = (44, 66)
        with self.assertRaisesRegex(r.Rejected, "cgroup-parent-replacement"):
            host.group_state()

    def test_cleanup_kills_entire_group_then_checks_removed_original_leaf(self):
        host, _, _ = self.fixture()
        host.paths["ctl"] = "/nix/store/fixed/systemctl"
        host.command = Mock()
        exited = dict(properties(), MainPID="0", SubState="exited", ControlGroup="")
        stopped = dict(
            exited, LoadState="not-found", InvocationID="", ActiveState="inactive"
        )
        host.show.side_effect = [properties(), exited, stopped]
        host.dead.side_effect = [False, True, True, True]
        host.quiesced.side_effect = [False, True, True, True]
        with patch.object(
            host, "same_group", side_effect=["present", "removed", "removed"]
        ):
            host.cleanup()
        calls = [call.args[0] for call in host.command.call_args_list]
        self.assertIn("--kill-whom=all", calls[0])
        self.assertIn("--signal=SIGKILL", calls[0])
        self.assertIn("stop", calls[1])

    def test_cleanup_timeout_after_whole_group_kill_never_succeeds(self):
        host, _, _ = self.fixture()
        host.paths["ctl"] = "/nix/store/fixed/systemctl"
        host.command = Mock()
        host.tick = Mock(side_effect=r.Rejected("deadline"))
        host.quiesced.return_value = False
        with self.assertRaisesRegex(r.Rejected, "deadline"):
            host.cleanup()
        host.command.assert_called_once()
        self.assertIn("--kill-whom=all", host.command.call_args.args[0])


class LifecycleTests(unittest.TestCase):
    def host(self):
        host = Mock()
        host.summary.return_value = {"fixed": "observation"}
        host.quiescence_evidence = "events-populated-0"
        return host

    def test_success_requires_cleanup(self):
        host = self.host()
        self.assertTrue(r.inspect(host)["ok"])
        self.assertEqual(
            [c[0] for c in host.method_calls],
            ["launch", "pin", "release", "wait_exit", "cleanup", "close", "summary"],
        )

    def test_both_quiescence_paths_leave_kernel_reclamation_unverified(self):
        for evidence in (
            "events-populated-0",
            "removed-guard-dead-reclamation-unverified",
        ):
            host = self.host()
            host.quiescence_evidence = evidence
            result = r.inspect(host)
            self.assertTrue(result["ok"])
            self.assertTrue(result["userspace_quiesced"])
            self.assertEqual(result["quiescence_evidence"], evidence)
            self.assertEqual(result["kernel_reclamation"], "unverified")
            self.assertNotIn("cleanup_verified", result)

    def test_collision_or_unknown_launch_does_not_signal_or_retry(self):
        for error in (
            r.Rejected("command-failed"),
            KeyboardInterrupt(),
            r.Rejected("command-output-cap"),
        ):
            host = self.host()
            host.launch.side_effect = error
            self.assertFalse(r.inspect(host)["ok"])
            host.launch.assert_called_once()
            host.cleanup.assert_not_called()
            host.release.assert_not_called()

    def test_deadline_identity_failure_and_interrupt_cleanup(self):
        for stage in ("pin", "release", "wait_exit"):
            for error in (
                r.Rejected("deadline"),
                r.Rejected("unit-replacement"),
                KeyboardInterrupt(),
            ):
                host = self.host()
                getattr(host, stage).side_effect = error
                result = r.inspect(host)
                self.assertFalse(result["ok"])
                self.assertTrue(result["userspace_quiesced"])
                host.cleanup.assert_called_once()

    def test_cleanup_interruption_missing_events_and_populated_are_failures(self):
        for error in (
            KeyboardInterrupt(),
            FileNotFoundError(),
            r.Rejected("cleanup-cgroup-populated"),
        ):
            host = self.host()
            host.cleanup.side_effect = error
            result = r.inspect(host)
            self.assertEqual(
                result,
                {
                    "ok": False,
                    "kernel_reclamation": "unverified",
                    "userspace_quiesced": False,
                    "quiescence_evidence": "events-populated-0",
                    "check": "cleanup-unverified",
                    "cleanup_failure": r.failure_name(error, "cleanup-unverified"),
                },
            )

    def test_primary_failure_survives_cleanup_and_descriptor_errors(self):
        host = self.host()
        host.pin.side_effect = r.Rejected("property-fields")
        host.cleanup.side_effect = r.Rejected("cleanup-ownership-unknown")
        host.close.side_effect = OSError("private host diagnostic")
        result = r.inspect(host)
        self.assertEqual(result["primary_failure"], "property-fields")
        self.assertEqual(result["cleanup_failure"], "cleanup-ownership-unknown")
        self.assertEqual(result["check"], "cleanup-unverified")
        self.assertFalse(result["ok"])
        self.assertNotIn("private", json.dumps(result))

    def test_uncontrolled_errors_do_not_reach_report(self):
        host = self.host()
        host.pin.side_effect = r.Rejected("private host diagnostic\n")
        host.cleanup.side_effect = OSError("secret endpoint")
        result = r.inspect(host)
        self.assertEqual(result["primary_failure"], "host-observation-failed")
        self.assertEqual(result["cleanup_failure"], "cleanup-unverified")
        self.assertNotIn("secret", json.dumps(result))
        self.assertNotIn("private", json.dumps(result))

    def test_command_deadline_starts_no_process(self):
        host = r.Host({}, UNIT)
        host.deadline = 0
        with (
            patch.object(r.subprocess, "Popen") as popen,
            self.assertRaises(r.Rejected),
        ):
            host.command(["fixed"])
        popen.assert_not_called()

    def test_unreviewed_entrypoint_is_blocked_before_command(self):
        host = r.Host({}, UNIT)
        with patch.object(r.subprocess, "Popen") as popen:
            result = r.inspect(host)
        self.assertEqual(result["check"], "independent-lifecycle-review-required")
        self.assertFalse(result["ok"])
        popen.assert_not_called()

    def test_command_output_cap_kills_only_client(self):
        host = r.Host({}, UNIT)
        child = Mock()
        child.poll.return_value = None
        child.wait.return_value = 0
        child.pid = 999
        poll = Mock()
        poll.__enter__ = Mock(return_value=poll)
        poll.__exit__ = Mock(return_value=None)
        poll.get_map.return_value = {10: 1}
        poll.select.return_value = [
            (SimpleNamespace(fd=10, fileobj=child.stdout), None)
        ]
        total = 0

        def read(_fd, maximum):
            nonlocal total
            total += maximum
            return b"x" * maximum

        with (
            patch.object(r.subprocess, "Popen", return_value=child),
            patch.object(r.selectors, "DefaultSelector", return_value=poll),
            patch.object(r.os, "set_blocking"),
            patch.object(r.os, "read", side_effect=read),
            patch.object(r.os, "killpg") as kill,
            self.assertRaisesRegex(r.Rejected, "command-output-cap"),
        ):
            host.command(["fixed"])
        self.assertEqual(total, r.CAP + 1)
        kill.assert_called_once_with(999, r.signal.SIGKILL)


if __name__ == "__main__":
    unittest.main()
