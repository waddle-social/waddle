"""Fixed benign resource inspection. No payload, endpoint or path arguments."""

import fcntl
import hashlib
import json
import os
import re
import select
import selectors
import signal
import stat
import subprocess
import sys
import time
from pathlib import Path

CAP = 8192
MAX_UNIT_PATHS = 64
PHASE_OUTPUT_CAP = 32768
SECONDS = 6.0
CLEANUP_SECONDS = 3.0
MAX_REPORT = 2048
REPORTING_EXIT = 74
PROPERTIES = {
    "Type": "exec",
    "Restart": "no",
    "RemainAfterExit": "yes",
    "KillMode": "control-group",
    "KillSignal": "SIGKILL",
    "FinalKillSignal": "SIGKILL",
    "SendSIGKILL": "yes",
    "RuntimeMaxSec": "8s",
    "RuntimeRandomizedExtraSec": "0",
    "TimeoutStartSec": "2s",
    "TimeoutStopSec": "1s",
    "CPUQuota": "50%",
    "CPUQuotaPeriodSec": "100ms",
    "MemoryAccounting": "yes",
    "MemoryMax": "67108864",
    "MemorySwapMax": "0",
    "TasksAccounting": "yes",
    "TasksMax": "8",
    "Delegate": "no",
    "LimitCORE": "0",
    "LimitFSIZE": "0",
    "NoNewPrivileges": "yes",
    "StandardInput": "null",
    "StandardOutput": "null",
    "StandardError": "null",
    "WorkingDirectory": "/",
}
READBACK = {
    key: value
    for key, value in PROPERTIES.items()
    if key
    not in {
        "RuntimeMaxSec",
        "RuntimeRandomizedExtraSec",
        "TimeoutStartSec",
        "TimeoutStopSec",
        "CPUQuota",
        "CPUQuotaPeriodSec",
    }
}
READBACK.update(
    {
        "RuntimeMaxUSec": "8s",
        "RuntimeRandomizedExtraUSec": "0",
        "TimeoutStartUSec": "2s",
        "TimeoutStopUSec": "1s",
        "CPUQuotaPerSecUSec": "500ms",
        "CPUQuotaPeriodUSec": "100ms",
        "LimitCORESoft": "0",
        "LimitFSIZESoft": "0",
        "Transient": "yes",
        "SourcePath": "",
        "DropInPaths": "",
        # systemctl show's generic INT32 printer uses decimal signal numbers.
        "KillSignal": "9",
        "FinalKillSignal": "9",
    }
)
FIELDS = tuple(READBACK) + (
    "Id",
    "LoadState",
    "ActiveState",
    "SubState",
    "InvocationID",
    "MainPID",
    "ExecMainPID",
    "ControlGroup",
    "Result",
    "ExecMainCode",
    "ExecMainStatus",
    "FragmentPath",
)
LIMITS = {
    "cpu.max": b"50000 100000\n",
    "memory.max": b"67108864\n",
    "memory.swap.max": b"0\n",
    "pids.max": b"8\n",
}


class Rejected(Exception):
    """Only fixed check names reach the report."""


def require(value, check):
    if not value:
        raise Rejected(check)


def failure_name(error, fallback):
    if isinstance(error, Rejected) and re.fullmatch(r"[a-z0-9-]{1,80}", str(error)):
        return str(error)
    return fallback


class PipeReports:
    """One atomic, nonblocking report attempt; no write-error fallback."""

    def __init__(self, fds=(1, 2)):
        self.fds = fds
        self.pipes = {}

    def pipe(self, fd):
        info = os.fstat(fd)
        require(stat.S_ISFIFO(info.st_mode), "output-pipes-required")
        flags = fcntl.fcntl(fd, fcntl.F_GETFL)
        require(
            flags & os.O_ACCMODE in {os.O_WRONLY, os.O_RDWR}, "output-pipes-required"
        )
        require(os.fpathconf(fd, "PC_PIPE_BUF") >= MAX_REPORT, "output-pipes-required")
        fcntl.fcntl(fd, fcntl.F_SETFL, flags | os.O_NONBLOCK)
        poll = select.poll()
        poll.register(
            fd, select.POLLOUT | select.POLLERR | select.POLLHUP | select.POLLNVAL
        )
        events = poll.poll(0)
        require(
            len(events) == 1
            and events[0][1] & select.POLLOUT
            and not events[0][1] & (select.POLLERR | select.POLLHUP | select.POLLNVAL),
            "output-pipes-required",
        )
        return info.st_dev, info.st_ino

    def validate(self):
        self.pipes = {}
        for fd in self.fds:
            try:
                self.pipes[fd] = self.pipe(fd)
            except (Rejected, OSError, ValueError):
                continue
        require(len(self.pipes) == len(self.fds) == 2, "output-pipes-required")

    def send(self, result):
        # On validation failure, an independently validated stderr pipe may
        # receive the one report. Never try a second FD after a report fails.
        fd = next((fd for fd in self.fds if fd in self.pipes), None)
        if fd is None:
            return False
        try:
            data = (
                json.dumps(result, sort_keys=True, separators=(",", ":")).encode(
                    "ascii"
                )
                + b"\n"
            )
            require(len(data) <= MAX_REPORT, "report-size")
            require(self.pipe(fd) == self.pipes[fd], "report-pipe-replaced")
            return os.write(fd, data) == len(data)
        except (
            Rejected,
            OSError,
            ValueError,
            TypeError,
            RuntimeError,
            UnicodeError,
            KeyboardInterrupt,
            SystemExit,
        ):
            return False


def number(value):
    require(re.fullmatch(r"[0-9]{1,10}", value), "pid-format")
    result = int(value)
    require(1 < result < 2**31, "pid-format")
    return result


def parse(data):
    require(type(data) is bytes and 0 < len(data) <= CAP, "property-size")
    try:
        lines = data.decode("ascii").splitlines()
    except UnicodeError:
        raise Rejected("property-format") from None
    result = {}
    for line in lines:
        key, sep, value = line.partition("=")
        require(sep and key in FIELDS and key not in result, "property-format")
        result[key] = value
    require(result.keys() == set(FIELDS), "property-fields")
    return result


def controls(props, unit):
    require(props["Id"] == unit and props["LoadState"] == "loaded", "unit-identity")
    require(all(props[k] == v for k, v in READBACK.items()), "unit-controls")
    require(re.fullmatch(r"[0-9a-f]{32}", props["InvocationID"]), "invocation-id")
    require(props["InvocationID"] != "0" * 32, "invocation-id")


def process_stat(data):
    require(len(data) <= CAP, "process-size")
    fields = data.rpartition(b") ")[2].split()
    require(len(fields) >= 20, "process-format")
    require(re.fullmatch(rb"[0-9]{1,20}", fields[19]), "process-format")
    return fields[0], int(fields[19])


def read_at(fd, name, maximum=CAP):
    child = os.open(name, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW, dir_fd=fd)
    try:
        data = os.read(child, maximum + 1)
        require(len(data) <= maximum, "observation-size")
        return data
    finally:
        os.close(child)


def file_identity(fd):
    value = os.fstat(fd)
    return value.st_dev, value.st_ino


def open_group(path):
    require(
        re.fullmatch(r"/[A-Za-z0-9_.@:-]+(?:/[A-Za-z0-9_.@:-]+)*", path), "cgroup-path"
    )
    require(all(p not in {".", ".."} for p in path.split("/")[1:]), "cgroup-path")
    fd = os.open(
        "/sys/fs/cgroup", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
    )
    try:
        for component in path.split("/")[1:]:
            next_fd = os.open(
                component,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=fd,
            )
            os.close(fd)
            fd = next_fd
        return fd
    except BaseException:
        os.close(fd)
        raise


def empty_events(data):
    require(len(data) <= 128, "cgroup-events-size")
    entries = {}
    for line in data.splitlines():
        parts = line.split()
        require(len(parts) == 2 and parts[0] not in entries, "cgroup-events-format")
        entries[parts[0]] = parts[1]
    require(entries.get(b"populated") in {b"0", b"1"}, "cgroup-events-format")
    return entries[b"populated"] == b"0"


def unit_paths(data):
    """Reject ambiguous CLI escaping instead of guessing at manager paths."""
    require(type(data) is bytes and len(data) <= CAP, "manager-unit-path-size")
    try:
        line = data.decode("ascii")
    except UnicodeError:
        raise Rejected("manager-unit-path-format") from None
    require(
        line.startswith("UnitPath=") and line.endswith("\n") and line.count("\n") == 1,
        "manager-unit-path-format",
    )
    roots = tuple(line[len("UnitPath=") : -1].split(" "))
    require(
        0 < len(roots) <= MAX_UNIT_PATHS and len(set(roots)) == len(roots),
        "manager-unit-path-format",
    )
    for root in roots:
        require(
            len(root) <= 512
            and re.fullmatch(r"/(?:[A-Za-z0-9_.@:+-]+(?:/[A-Za-z0-9_.@:+-]+)*)?", root),
            "manager-unit-path-format",
        )
        require(
            all(part not in {".", ".."} for part in root.split("/")[1:]),
            "manager-unit-path-format",
        )
    return roots


def configuration_names(unit):
    require(
        re.fullmatch(r"waddle-resource-[0-9]+-[0-9a-f]{32}\.service", unit),
        "fixed-unit-name",
    )
    stem = unit.removesuffix(".service")
    prefixes = [stem[: match.end()] + ".service" for match in re.finditer("-", stem)]
    # Include the exact unit file/alias as well as a conservative superset of
    # dashed-prefix drop-ins and dependencies, even if some suffixes don't apply.
    return (unit, "service.d") + tuple(
        name + suffix
        for name in (*prefixes, unit)
        for suffix in (".d", ".wants", ".requires", ".upholds")
    )


def configuration_snapshot(roots, unit, tick, allow_fragment=None):
    """Inspect current paths only; this is explicitly not atomic with launch."""
    result = []
    for root in roots:
        current = Path("/")
        fd = os.open(
            current, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
        )
        try:
            walked = [(str(current), file_identity(fd))]
        finally:
            os.close(fd)
        for component in root.split("/")[1:]:
            tick()
            if not component:
                continue
            candidate = current / component
            try:
                item = candidate.lstat()
            except FileNotFoundError:
                # An absent ordinary root is decidable through its last existing
                # readable ancestor. A dangling symlink is rejected below.
                result.append((root, "absent", str(candidate), tuple(walked)))
                break
            if stat.S_ISLNK(item.st_mode):
                try:
                    candidate = candidate.resolve(strict=True)
                except (OSError, RuntimeError):
                    raise Rejected("unit-path-unresolved") from None
            current = candidate
            fd = os.open(
                current, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
            )
            try:
                walked.append((str(current), file_identity(fd)))
            finally:
                os.close(fd)
        else:
            fd = os.open(
                current, os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
            )
            try:
                for name in configuration_names(unit):
                    tick()
                    try:
                        os.stat(name, dir_fd=fd, follow_symlinks=False)
                    except FileNotFoundError:
                        continue
                    if name == unit and allow_fragment is not None:
                        allow_fragment(root, fd)
                        continue
                    raise Rejected("inherited-unit-configuration")
                result.append((root, "present", str(current), tuple(walked)))
            finally:
                os.close(fd)
    return tuple(result)


def lifecycle_review_required():
    """Manager configuration and lifecycle activation remain under review."""
    raise Rejected("independent-lifecycle-review-required")


def transient_directory():
    # Supported host assumption: the user manager uses this XDG_RUNTIME_DIR,
    # no --test/root/tempdir override, and reports this exact root in UnitPath.
    # systemd v261.2 path-lookup.c:238-251 derives systemd/transient from it.
    return f"/run/user/{os.getuid()}/systemd/transient"


def open_directory(path):
    fd = os.open("/", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC)
    try:
        for component in path.split("/")[1:]:
            require(component not in {"", ".", ".."}, "transient-directory-path")
            next_fd = os.open(
                component,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=fd,
            )
            os.close(fd)
            fd = next_fd
        return fd
    except BaseException:
        os.close(fd)
        raise


class Host:
    def __init__(self, paths, unit):
        self.paths, self.unit = paths, unit
        self.deadline = time.monotonic() + SECONDS
        self.budget = PHASE_OUTPUT_CAP
        self.pidfd = self.procfd = self.cgfd = self.events = self.parentfd = None
        self.saved = None
        self.identity = None
        self.parent_identity = None
        self.start = None
        self.quiescence_evidence = None
        self.configuration = None
        self.runtime_path = transient_directory()
        self.runtimefd = self.fragmentfd = None
        self.runtime_identity = self.fragment_identity = None
        self.fragment_invocation = None
        self.fragment_present = False
        self.launch_known = self.stop_sent = self.unit_stopped = False
        self.env = {
            "XDG_RUNTIME_DIR": f"/run/user/{os.getuid()}",
            "DBUS_SESSION_BUS_ADDRESS": f"unix:path=/run/user/{os.getuid()}/bus",
            "LC_ALL": "C",
            "SYSTEMD_LOG_LEVEL": "emerg",
            "SYSTEMD_LOG_TARGET": "null",
            "SYSTEMD_PAGER": "",
            "SYSTEMD_COLORS": "0",
        }

    def tick(self):
        require(time.monotonic() < self.deadline, "deadline")

    def command(self, argv):
        self.tick()
        child = subprocess.Popen(
            argv,
            env=self.env,
            cwd="/",
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            close_fds=True,
        )
        output = bytearray()
        call_budget = CAP
        end = min(self.deadline, time.monotonic() + 1.0)
        try:
            with selectors.DefaultSelector() as poll:
                for stream in (child.stdout, child.stderr):
                    os.set_blocking(stream.fileno(), False)
                    poll.register(stream, selectors.EVENT_READ)
                while poll.get_map() or child.poll() is None:
                    require(time.monotonic() < end, "command-deadline")
                    for key, _ in poll.select(min(0.01, end - time.monotonic())):
                        data = os.read(
                            key.fd, min(1024, self.budget + 1, call_budget + 1)
                        )
                        self.budget -= len(data)
                        call_budget -= len(data)
                        require(
                            self.budget >= 0 and call_budget >= 0, "command-output-cap"
                        )
                        if not data:
                            poll.unregister(key.fileobj)
                        elif key.fileobj is child.stdout:
                            output.extend(data)
            require(child.wait(timeout=0) == 0, "command-failed")
            return bytes(output)
        finally:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait(timeout=0.2)
            child.stdout.close()
            child.stderr.close()

    def show(self):
        return parse(
            self.command(
                [
                    self.paths["ctl"],
                    "--user",
                    "show",
                    "--no-pager",
                    "--all",
                    "--no-ask-password",
                    "--property=" + ",".join(FIELDS),
                    "--",
                    self.unit,
                ]
            )
        )

    def launch(self):
        lifecycle_review_required()
        self.configuration = self.configuration_now()
        self.pin_runtime(self.configuration[0])
        self.verify_configuration()
        # The preflight has its own bounded output phase, then inspection starts.
        self.budget = PHASE_OUTPUT_CAP
        # --unit plus systemd-run's fixed StartTransientUnit mode=fail refuses reuse.
        self.command(
            [
                self.paths["run"],
                "--user",
                "--quiet",
                "--no-ask-password",
                "--job-mode=fail",
                "--unit=" + self.unit,
                *("--property=" + k + "=" + v for k, v in PROPERTIES.items()),
                "--",
                self.paths["guard"],
            ]
        )
        self.launch_known = True

    def pin_runtime(self, roots):
        require(self.runtime_path in roots, "transient-directory-not-in-unit-path")
        self.runtimefd = open_directory(self.runtime_path)
        info = os.fstat(self.runtimefd)
        require(
            stat.S_ISDIR(info.st_mode) and info.st_uid == os.getuid(),
            "transient-directory-owner",
        )
        self.runtime_identity = file_identity(self.runtimefd)

    def current_runtime(self):
        require(
            self.runtimefd is not None
            and file_identity(self.runtimefd) == self.runtime_identity,
            "transient-directory-replacement",
        )
        fd = open_directory(self.runtime_path)
        try:
            info = os.fstat(fd)
            require(
                file_identity(fd) == self.runtime_identity
                and info.st_uid == os.getuid(),
                "transient-directory-replacement",
            )
        finally:
            os.close(fd)

    def fragment_bytes(self, parent):
        fd = os.open(
            self.unit,
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW | os.O_NONBLOCK,
            dir_fd=parent,
        )
        try:
            before = os.fstat(fd)
            require(
                stat.S_ISREG(before.st_mode)
                and before.st_uid == os.getuid()
                and before.st_nlink == 1
                and 0 < before.st_size <= CAP,
                "transient-fragment-file",
            )
            data = os.read(fd, CAP + 1)
            after = os.fstat(fd)

            def identity(info):
                return (
                    info.st_dev,
                    info.st_ino,
                    info.st_uid,
                    info.st_mode,
                    info.st_nlink,
                    info.st_size,
                    info.st_mtime_ns,
                    info.st_ctime_ns,
                )

            require(
                len(data) == before.st_size <= CAP
                and identity(before) == identity(after),
                "transient-fragment-mutation",
            )
            return fd, (identity(after), hashlib.sha256(data).hexdigest())
        except BaseException:
            os.close(fd)
            raise

    def pin_fragment(self, props):
        require(
            self.launch_known and self.fragmentfd is None,
            "transient-fragment-ownership",
        )
        controls(props, self.unit)
        require(
            props["FragmentPath"] == self.runtime_path + "/" + self.unit,
            "transient-fragment-path",
        )
        self.current_runtime()
        self.fragmentfd, self.fragment_identity = self.fragment_bytes(self.runtimefd)
        self.fragment_invocation = props["InvocationID"]
        self.fragment_present = True

    def fragment_properties(self, props, allow_missing=False):
        require(
            self.launch_known and self.fragmentfd is not None,
            "transient-fragment-ownership",
        )
        if allow_missing and props["LoadState"] == "not-found":
            require(
                self.stop_sent
                and self.unit_stopped
                and self.dead()
                and self.quiesced()
                and props["Id"] == self.unit
                and props["MainPID"] == "0"
                and props["FragmentPath"]
                == props["SourcePath"]
                == props["DropInPaths"]
                == ""
                and props["InvocationID"] == ""
                and props["Transient"] == "no",
                "transient-fragment-properties",
            )
        else:
            controls(props, self.unit)
            require(
                props["FragmentPath"] == self.runtime_path + "/" + self.unit
                and props["InvocationID"] == self.fragment_invocation,
                "transient-fragment-properties",
            )

    def check_fragment(self, allow_missing=False):
        self.current_runtime()
        try:
            fd, observed = self.fragment_bytes(self.runtimefd)
        except FileNotFoundError:
            require(
                allow_missing
                and self.stop_sent
                and self.unit_stopped
                and self.dead()
                and self.quiesced(),
                "transient-fragment-disappeared",
            )
            self.fragment_present = False
            return
        try:
            require(
                observed == self.fragment_identity
                and file_identity(self.fragmentfd) == observed[0][:2],
                "transient-fragment-replacement-or-mutation",
            )
        finally:
            os.close(fd)
        self.fragment_present = True

    def configuration_now(self, props=None, allow_missing=False):
        roots = unit_paths(
            self.command(
                [
                    self.paths["ctl"],
                    "--user",
                    "show",
                    "--no-pager",
                    "--all",
                    "--no-ask-password",
                    "--property=UnitPath",
                ]
            )
        )
        if self.runtimefd is not None:
            require(self.runtime_path in roots, "transient-directory-not-in-unit-path")
            self.current_runtime()
        allow = None
        if self.launch_known:
            self.fragment_properties(
                props if props is not None else self.show(), allow_missing
            )
            self.check_fragment(allow_missing)

            def allow(root, fd):
                require(
                    root == self.runtime_path
                    and file_identity(fd) == self.runtime_identity,
                    "unexpected-transient-fragment",
                )
                self.check_fragment(allow_missing)

        snapshot = configuration_snapshot(roots, self.unit, self.tick, allow)
        if self.launch_known:
            self.check_fragment(allow_missing)
        return roots, snapshot

    def verify_configuration(self, props=None, allow_missing=False):
        require(self.configuration is not None, "unit-configuration-not-pinned")
        require(
            self.configuration_now(props, allow_missing) == self.configuration,
            "manager-unit-path-drift",
        )

    def pin(self):
        props = self.show()
        controls(props, self.unit)
        self.pin_fragment(props)
        require(
            props["ActiveState"] == "active" and props["SubState"] == "running",
            "unit-state",
        )
        pid = number(props["MainPID"])
        require(props["ExecMainPID"] == str(pid), "main-pid")
        path = props["ControlGroup"]
        require(
            path.startswith(
                f"/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service/"
            )
            and path.endswith("/" + self.unit),
            "cgroup-owner",
        )
        self.pidfd = os.pidfd_open(pid, 0)
        self.procfd = os.open(
            f"/proc/{pid}", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW
        )
        self.cgfd = open_group(path)
        self.parentfd = open_group(path.rpartition("/")[0])
        self.parent_identity = file_identity(self.parentfd)
        self.events = os.open(
            "cgroup.events",
            os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW,
            dir_fd=self.cgfd,
        )
        self.identity = file_identity(self.cgfd)
        self.saved = props
        _, self.start = process_stat(read_at(self.procfd, "stat"))
        self.check()

    def dead(self):
        return bool(select.select([self.pidfd], [], [], 0)[0])

    def group_state(self):
        """Anchor absence to the held parent, never to an unanchored pathname."""
        require(
            file_identity(self.parentfd) == self.parent_identity,
            "cgroup-parent-replacement",
        )
        current_parent = open_group(self.saved["ControlGroup"].rpartition("/")[0])
        try:
            require(
                file_identity(current_parent) == self.parent_identity,
                "cgroup-parent-replacement",
            )
        finally:
            os.close(current_parent)
        require(file_identity(self.cgfd) == self.identity, "cgroup-replacement")
        try:
            fresh = os.open(
                self.unit,
                os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
                dir_fd=self.parentfd,
            )
        except FileNotFoundError:
            require(self.dead(), "cgroup-removed-while-guard-alive")
            return "removed"
        try:
            require(file_identity(fresh) == self.identity, "cgroup-replacement")
        finally:
            os.close(fresh)
        return "present"

    def same_group(self, props):
        state = self.group_state()
        require(
            props["ControlGroup"] == self.saved["ControlGroup"]
            or (state == "removed" and props["ControlGroup"] == ""),
            "cgroup-replacement",
        )
        return state

    def check(self):
        self.tick()
        props = self.show()
        controls(props, self.unit)
        if self.launch_known:
            self.fragment_properties(props)
            self.check_fragment()
        for key in ("InvocationID", "ControlGroup", "MainPID", "ExecMainPID"):
            require(props[key] == self.saved[key], "unit-replacement")
        require(
            props["ActiveState"] == "active" and props["SubState"] == "running",
            "unit-state",
        )
        require(not self.dead(), "guard-exited")
        state, start = process_stat(read_at(self.procfd, "stat"))
        require(
            state == b"T" and start == self.start and start > 0,
            "guard-replacement-or-barrier",
        )
        require(
            read_at(self.procfd, "cgroup")
            == ("0::" + props["ControlGroup"] + "\n").encode(),
            "guard-cgroup",
        )
        require(read_at(self.procfd, "environ", 1) == b"", "guard-environment")
        status = read_at(self.procfd, "status")
        uid = re.findall(
            rb"^Uid:\s+([0-9]+)\s+([0-9]+)\s+([0-9]+)\s+([0-9]+)$", status, re.MULTILINE
        )
        require(uid == [(str(os.getuid()).encode(),) * 4], "guard-uid")
        require(b"NoNewPrivs:\t1\n" in status, "guard-no-new-privileges")
        require(b"Threads:\t1\n" in status, "guard-thread-count")
        exe = os.stat("exe", dir_fd=self.procfd)
        guard = os.stat(self.paths["guard"])
        require(
            stat.S_ISREG(exe.st_mode)
            and (exe.st_dev, exe.st_ino) == (guard.st_dev, guard.st_ino),
            "guard-executable",
        )
        require(
            set(os.listdir(self.procfd_path("fd"))) == {"0", "1", "2"},
            "guard-descriptors",
        )
        for fd in ("0", "1", "2"):
            dev = os.stat(self.procfd_path("fd/" + fd))
            require(
                stat.S_ISCHR(dev.st_mode) and dev.st_rdev == os.makedev(1, 3),
                "guard-stdio",
            )
        require(self.group_state() == "present", "cgroup-removed-at-barrier")
        require(read_at(self.cgfd, "cgroup.type") == b"domain\n", "cgroup-type")
        require(
            set(read_at(self.cgfd, "cgroup.controllers").split())
            >= {b"cpu", b"memory", b"pids"},
            "cgroup-controllers",
        )
        require(
            read_at(self.cgfd, "cgroup.procs") == (props["MainPID"] + "\n").encode(),
            "cgroup-members",
        )
        require({k: read_at(self.cgfd, k) for k in LIMITS} == LIMITS, "cgroup-limits")
        require(not self.quiesced(), "cgroup-empty-at-barrier")
        require(not self.dead(), "guard-exited")

    def procfd_path(self, suffix):
        return f"/proc/self/fd/{self.procfd}/{suffix}"

    def quiesced(self):
        # Removal is only fixed-guard userspace quiescence. The running Zen
        # kernel may remove PF_EXITING tasks before the population count clears.
        if self.group_state() == "removed":
            self.quiescence_evidence = "removed-guard-dead-reclamation-unverified"
            return True
        try:
            os.lseek(self.events, 0, os.SEEK_SET)
            empty = empty_events(os.read(self.events, 129))
        except OSError:
            # The manager can prune between the directory lookup and this read.
            require(self.group_state() == "removed", "cgroup-events-unreadable")
            self.quiescence_evidence = "removed-guard-dead-reclamation-unverified"
            return True
        if empty:
            self.quiescence_evidence = "events-populated-0"
        return empty

    def release(self):
        self.verify_configuration()
        self.check()
        signal.pidfd_send_signal(self.pidfd, signal.SIGCONT)

    def published_exit(self):
        """A dead pidfd can precede manager publication; never retry drift."""
        require(self.dead(), "guard-still-alive")
        while True:
            self.tick()
            props = self.show()
            controls(props, self.unit)
            if self.launch_known:
                self.fragment_properties(props)
                self.check_fragment()
            require(
                props["InvocationID"] == self.saved["InvocationID"]
                and props["ExecMainPID"] == self.saved["MainPID"],
                "unit-replacement",
            )
            require(
                props["MainPID"] in {"0", self.saved["MainPID"]}, "unit-replacement"
            )
            self.same_group(props)
            if props["MainPID"] == "0" and (
                props["ActiveState"],
                props["SubState"],
            ) != ("active", "running"):
                return props
            require(
                (props["ActiveState"], props["SubState"]) == ("active", "running"),
                "unit-state",
            )
            time.sleep(0.01)

    def wait_exit(self):
        while not self.dead():
            self.tick()
            time.sleep(0.01)
        props = self.published_exit()
        require(self.quiesced(), "cgroup-still-populated")
        require(
            props["MainPID"] == "0"
            and props["ExecMainPID"] == self.saved["MainPID"]
            and props["ActiveState"] == "active"
            and props["SubState"] == "exited"
            and props["Result"] == "success"
            and props["ExecMainCode"] == "1"
            and props["ExecMainStatus"] == "0",
            "guard-exit-result",
        )

    def cleanup(self):
        # A separate bounded budget is reserved even after inspection fails.
        self.deadline = time.monotonic() + CLEANUP_SECONDS
        self.budget = PHASE_OUTPUT_CAP
        require(
            self.saved is not None and self.events is not None,
            "cleanup-ownership-unknown",
        )
        self.verify_configuration()
        props = self.show()
        controls(props, self.unit)
        require(
            props["InvocationID"] == self.saved["InvocationID"],
            "cleanup-unit-replacement",
        )
        require(
            props["MainPID"] in {"0", self.saved["MainPID"]}, "cleanup-pid-replacement"
        )
        self.same_group(props)
        if props["MainPID"] != "0":
            if self.dead():
                props = self.published_exit()
            else:
                _, start = process_stat(read_at(self.procfd, "stat"))
                require(start == self.start, "cleanup-pid-replacement")
        # Kill the entire group, including descendants; never signal a guessed PID.
        if not self.quiesced():
            self.command(
                [
                    self.paths["ctl"],
                    "--user",
                    "kill",
                    "--kill-whom=all",
                    "--signal=SIGKILL",
                    "--",
                    self.unit,
                ]
            )
        while not (self.dead() and self.quiesced()):
            self.tick()
            time.sleep(0.01)
        # Removal does not establish aggregate cleanup or kernel reclamation.
        self.published_exit()
        self.command(
            [self.paths["ctl"], "--user", "--no-block", "stop", "--", self.unit]
        )
        self.stop_sent = True
        require(self.quiesced(), "cleanup-cgroup-populated")
        while True:
            props = self.show()
            require(
                props["InvocationID"] in {"", self.saved["InvocationID"]},
                "cleanup-unit-replacement",
            )
            require(props["MainPID"] == "0", "cleanup-pid-replacement")
            self.same_group(props)
            if props["LoadState"] == "not-found" or props["ActiveState"] in {
                "inactive",
                "failed",
            }:
                self.unit_stopped = True
                self.verify_configuration(props, allow_missing=True)
                break
            if self.launch_known:
                self.fragment_properties(props)
                self.check_fragment()
            self.tick()
            time.sleep(0.01)
        require(self.dead() and self.quiesced(), "cleanup-unverified")

    def summary(self):
        return {
            "invocation": self.saved["InvocationID"],
            "pid": number(self.saved["MainPID"]),
            "start_ticks": self.start,
            "cgroup": self.saved["ControlGroup"],
            "cgroup_device": self.identity[0],
            "cgroup_inode": self.identity[1],
            "limits": {k: v.decode().strip() for k, v in LIMITS.items()},
            "runtime_seconds": 8,
            "stop_seconds": 1,
            "quiescence_evidence": self.quiescence_evidence,
            "transient_fragment": "retained-after-stop"
            if self.fragment_present
            else "absent-after-stop",
        }

    def close(self):
        for fd in (
            self.events,
            self.cgfd,
            self.procfd,
            self.pidfd,
            self.parentfd,
            self.fragmentfd,
            self.runtimefd,
        ):
            if fd is not None:
                os.close(fd)


def inspect(host):
    launched = False
    success = False
    primary_failure = None
    cleanup_failure = None
    cleaned = False
    try:
        host.launch()
        launched = True
        host.pin()
        host.release()
        host.wait_exit()
        success = True
    except Rejected as error:
        primary_failure = failure_name(error, "host-observation-failed")
    except (OSError, ValueError, subprocess.SubprocessError):
        primary_failure = "host-observation-failed"
    except (KeyboardInterrupt, SystemExit):
        primary_failure = "interrupted"
    finally:
        if launched:
            try:
                host.cleanup()
                cleaned = True
            except Rejected as error:
                cleanup_failure = failure_name(error, "cleanup-unverified")
            except (
                OSError,
                ValueError,
                subprocess.SubprocessError,
                KeyboardInterrupt,
                SystemExit,
            ):
                cleanup_failure = "cleanup-unverified"
        try:
            host.close()
        except (OSError, ValueError, KeyboardInterrupt, SystemExit):
            cleanup_failure = cleanup_failure or "descriptor-cleanup-failed"
            cleaned = False
    evidence = getattr(host, "quiescence_evidence", None)
    result = {
        "ok": success and cleaned,
        "kernel_reclamation": "unverified",
        "userspace_quiesced": cleaned,
        "check": (
            "limits-observed"
            if evidence == "events-populated-0"
            else "limits-observed-reclamation-unverified"
        )
        if success and cleaned
        else "cleanup-unverified"
        if cleanup_failure
        else primary_failure,
    }
    if primary_failure is not None:
        result["primary_failure"] = primary_failure
    if cleanup_failure is not None:
        result["cleanup_failure"] = cleanup_failure
    if evidence in {"events-populated-0", "removed-guard-dead-reclamation-unverified"}:
        result["quiescence_evidence"] = evidence
    if result["ok"]:
        result["observed"] = host.summary()
    return result


def main():
    require(
        len(sys.argv) == 1 and os.getuid() != 0 and dict(os.environ) == {"LC_ALL": "C"},
        "fixed-entrypoint",
    )
    here = Path(__file__).resolve().parent
    require(str(here).startswith("/nix/store/"), "installed-package-required")
    # Generated at build time inside the immutable package; never caller input.
    paths = json.loads((here / "resource-paths.json").read_text())
    require(paths.keys() == {"run", "ctl", "guard"}, "fixed-paths")
    require(
        all(re.fullmatch(r"/nix/store/[A-Za-z0-9+._/-]+", p) for p in paths.values()),
        "fixed-paths",
    )
    unit = f"waddle-resource-{os.getuid()}-{os.urandom(16).hex()}.service"

    def interrupted(_signal, _frame):
        raise KeyboardInterrupt()

    for signum in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, interrupted)
    result = inspect(Host(paths, unit))
    result["unit"] = unit
    return result


def entrypoint():
    reports = PipeReports()
    valid_output = False
    try:
        signal.signal(signal.SIGPIPE, signal.SIG_IGN)
        # Must precede all configuration reads, manager queries and launches.
        reports.validate()
        valid_output = True
        result = main()
        code = 0 if result["ok"] else 1
    except Rejected as error:
        result = {"ok": False, "check": failure_name(error, "entrypoint-failed")}
        code = 1 if valid_output else REPORTING_EXIT
    except (
        OSError,
        ValueError,
        KeyError,
        TypeError,
        RuntimeError,
        subprocess.SubprocessError,
    ):
        result = {"ok": False, "check": "entrypoint-failed"}
        code = 1 if valid_output else REPORTING_EXIT
    except (KeyboardInterrupt, SystemExit):
        result = {"ok": False, "check": "interrupted"}
        code = 1 if valid_output else REPORTING_EXIT
    return code if reports.send(result) else REPORTING_EXIT


if __name__ == "__main__":
    # Avoid Python shutdown attempting a buffered flush to a broken report FD.
    os._exit(entrypoint())
