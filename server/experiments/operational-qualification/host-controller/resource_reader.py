"""Fixed future benign-observation reader. Activation and artifact pin are unset."""

import base64
import hashlib
import json
import os
import re
import resource
import selectors
import signal
import stat
import sys
import threading
import time

# A separately reviewed activation must replace these, never command-line input.
ARTIFACT = None
ARTIFACT_SHA256 = None
CAPTURE_SECONDS = 15.0
TERM_SECONDS = 3.0
KILL_SECONDS = 1.0
CHANNEL_CAP = 4096
TOTAL_CAP = 8192
REPORT_CAP = 2048
EVIDENCE_CAP = 16384
CHUNK = 1024


class Rejected(Exception):
    """Fixed errors only; child bytes and OS diagnostics never reach the terminal."""


def require(value, check):
    if not value:
        raise Rejected(check)


def activation_review_required():
    raise Rejected("reader-activation-review-required")


def identity(fd):
    info = os.fstat(fd)
    return info.st_dev, info.st_ino


def fixed_artifact():
    require(
        isinstance(ARTIFACT, str)
        and re.fullmatch(
            r"/nix/store/[0-9a-z]{32}-waddle-benign-resource-inspection-1/bin/benign-resource-inspection",
            ARTIFACT,
        )
        and isinstance(ARTIFACT_SHA256, str)
        and re.fullmatch(r"[0-9a-f]{64}", ARTIFACT_SHA256),
        "artifact-pin-required",
    )
    fd = os.open(ARTIFACT, os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        info = os.fstat(fd)
        require(
            stat.S_ISREG(info.st_mode)
            and info.st_mode & 0o222 == 0
            and 0 < info.st_size <= 1024 * 1024,
            "artifact-type",
        )
        digest = hashlib.sha256()
        remaining = info.st_size
        while remaining:
            block = os.read(fd, min(65536, remaining))
            require(block, "artifact-short-read")
            digest.update(block)
            remaining -= len(block)
        require(digest.hexdigest() == ARTIFACT_SHA256, "artifact-hash")
        current = os.fstat(fd)
        require(
            (
                current.st_dev,
                current.st_ino,
                current.st_size,
                current.st_mtime_ns,
                current.st_ctime_ns,
            )
            == (
                info.st_dev,
                info.st_ino,
                info.st_size,
                info.st_mtime_ns,
                info.st_ctime_ns,
            ),
            "artifact-drift",
        )
        return fd
    except BaseException:
        os.close(fd)
        raise


def child_exec(artifactfd):
    # Called only in the one forked child, after its parent's pidfd barrier.
    os.chdir("/")
    null = os.open("/dev/null", os.O_RDONLY | os.O_CLOEXEC)
    os.dup2(null, 0)
    os.close(null)
    # Linux execveat via Python's FD exec; no path is reopened after hashing.
    os.execve(artifactfd, [ARTIFACT], {"LC_ALL": "C"})


def spawn(artifactfd):
    """No wait/reap before pidfd ownership; no child exec before release."""
    require(threading.active_count() == 1, "single-reader-thread-required")
    require(
        signal.getsignal(signal.SIGCHLD) == signal.SIG_DFL, "sigchld-default-required"
    )
    require(os.execve in os.supports_fd, "fd-exec-required")
    # Keep freshly allocated pipe descriptors above the dup2 targets.
    for fd in (0, 1, 2):
        try:
            os.fstat(fd)
        except OSError:
            raise Rejected("reader-standard-fds-required") from None
    pipes = []
    pid = None
    try:
        for _ in range(3):
            pipes.extend(os.pipe2(os.O_CLOEXEC))
        out_r, out_w, err_r, err_w, start_r, start_w = pipes
        pid = os.fork()
        if pid == 0:
            try:
                for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
                    signal.signal(signum, signal.SIG_DFL)
                os.dup2(out_w, 1)
                os.dup2(err_w, 2)
                for fd in (out_r, out_w, err_r, err_w, start_w):
                    os.close(fd)
                if os.read(start_r, 1) != b"G":
                    os._exit(75)
                os.close(start_r)
                child_exec(artifactfd)
                os._exit(75)
            except (OSError, ValueError, RuntimeError, KeyboardInterrupt, SystemExit):
                os._exit(75)
        for fd in (out_w, err_w, start_r):
            os.close(fd)
            pipes.remove(fd)
        # Default SIGCHLD + own unreaped child keeps its PID unreusable here.
        os.set_blocking(out_r, False)
        os.set_blocking(err_r, False)
        pidfd = os.pidfd_open(pid, 0)
        return pidfd, out_r, err_r, start_w
    except BaseException:
        # EOF releases no executable. In particular, never guess a signal/reap
        # target if pidfd acquisition failed. Exit will reparent that child.
        for fd in pipes:
            os.close(fd)
        if pid is not None:
            raise Rejected("child-pin-failed-unreaped") from None
        raise


def no_duplicates(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, "report-duplicate-key")
        result[key] = value
    return result


def fixed_name(value):
    return isinstance(value, str) and re.fullmatch(r"[a-z0-9-]{1,80}", value)


def report(out, err, status):
    try:
        return parse_report(out, err, status)
    except (TypeError, KeyError, OverflowError):
        raise Rejected("report-schema") from None


def parse_report(out, err, status):
    require(not err, "report-stderr")
    require(out, "report-missing")
    require(len(out) <= REPORT_CAP, "report-size")
    require(out.endswith(b"\n"), "report-truncated")
    require(out.count(b"\n") == 1, "report-multiple")
    try:
        value = json.loads(out.decode("ascii"), object_pairs_hook=no_duplicates)
    except (ValueError, UnicodeError, RecursionError):
        raise Rejected("report-malformed") from None
    require(isinstance(value, dict) and type(value.get("ok")) is bool, "report-schema")
    require(status == (0 if value["ok"] else 1), "report-exit-status")
    require(fixed_name(value.get("check")), "report-check")
    allowed = {
        "ok",
        "check",
        "unit",
        "kernel_reclamation",
        "userspace_quiesced",
        "primary_failure",
        "cleanup_failure",
        "quiescence_evidence",
        "observed",
    }
    require(value.keys() <= allowed, "report-fields")
    for key in ("primary_failure", "cleanup_failure"):
        require(key not in value or fixed_name(value[key]), "report-failure")
    if "unit" in value:
        require(
            isinstance(value["unit"], str)
            and re.fullmatch(
                rf"waddle-resource-{os.getuid()}-[0-9a-f]{{32}}\.service", value["unit"]
            ),
            "report-unit",
        )
    evidence = value.get("quiescence_evidence")
    require(
        evidence
        in {None, "events-populated-0", "removed-guard-dead-reclamation-unverified"},
        "report-quiescence",
    )
    if "kernel_reclamation" in value:
        require(value["kernel_reclamation"] == "unverified", "report-reclamation")
    if "userspace_quiesced" in value:
        require(type(value["userspace_quiesced"]) is bool, "report-quiescence")
    if not value["ok"]:
        if "unit" not in value:
            require(value.keys() == {"ok", "check"}, "report-failure-fields")
            return value
        require(
            {"ok", "check", "unit", "kernel_reclamation", "userspace_quiesced"}
            <= value.keys(),
            "report-failure-fields",
        )
        require("observed" not in value, "report-failed-observations")
        require(value["kernel_reclamation"] == "unverified", "report-reclamation")
        if value["userspace_quiesced"]:
            require(
                evidence is not None and "cleanup_failure" not in value,
                "report-quiescence",
            )
        require(
            "primary_failure" in value or "cleanup_failure" in value,
            "report-failure-fields",
        )
        if "primary_failure" in value and "cleanup_failure" not in value:
            require(value["check"] == value["primary_failure"], "report-primary")
        if "cleanup_failure" in value:
            require(
                value.get("userspace_quiesced") is False
                and value["check"] == "cleanup-unverified",
                "report-cleanup",
            )
        return value
    require(
        value.keys()
        == {
            "ok",
            "check",
            "unit",
            "kernel_reclamation",
            "userspace_quiesced",
            "quiescence_evidence",
            "observed",
        }
        and value["userspace_quiesced"] is True
        and value["kernel_reclamation"] == "unverified"
        and evidence is not None,
        "report-success-fields",
    )
    require(
        value["check"]
        == (
            "limits-observed"
            if evidence == "events-populated-0"
            else "limits-observed-reclamation-unverified"
        ),
        "report-success-check",
    )
    observed = value["observed"]
    require(
        isinstance(observed, dict)
        and observed.keys()
        == {
            "invocation",
            "pid",
            "start_ticks",
            "cgroup",
            "cgroup_device",
            "cgroup_inode",
            "limits",
            "runtime_seconds",
            "stop_seconds",
            "quiescence_evidence",
            "transient_fragment",
        },
        "report-observed-fields",
    )
    require(
        isinstance(observed["invocation"], str)
        and re.fullmatch(r"[0-9a-f]{32}", observed["invocation"])
        and observed["invocation"] != "0" * 32,
        "report-invocation",
    )
    for key in ("pid", "start_ticks", "cgroup_inode", "cgroup_device"):
        require(
            type(observed[key]) is int
            and 0 <= observed[key] <= 2**64 - 1
            and (observed[key] > 0 or key == "cgroup_device"),
            "report-number",
        )
    require(
        observed["cgroup"]
        == f"/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service/app.slice/{value['unit']}",
        "report-cgroup",
    )
    require(
        observed["limits"]
        == {
            "cpu.max": "50000 100000",
            "memory.max": "67108864",
            "memory.swap.max": "0",
            "pids.max": "8",
        },
        "report-limits",
    )
    require(
        type(observed["runtime_seconds"]) is int
        and observed["runtime_seconds"] == 8
        and type(observed["stop_seconds"]) is int
        and observed["stop_seconds"] == 1
        and observed["quiescence_evidence"] == evidence
        and observed["transient_fragment"]
        in {"retained-after-stop", "absent-after-stop"},
        "report-observed-status",
    )
    return value


class Capture:
    """One fixed child, bounded memory/time, exact pidfd-only termination/reaping."""

    def __init__(self):
        self.interrupted = False

    def interrupt(self, _signum, _frame):
        self.interrupted = True

    def run(self, artifactfd):
        result = {
            "ok": False,
            "primary_failure": None,
            "cleanup_failure": None,
            "parent_reaped": False,
            "parent_pidfd_dead": False,
            "possible_unfinished_manager_operation": False,
            "kernel_reclamation": "unverified",
            "signals": [],
            "exit_status": None,
        }
        buffers = {"stdout": bytearray(), "stderr": bytearray()}
        eof = set()
        fds = []
        released = False
        selector = selectors.DefaultSelector()
        start = time.monotonic()
        cleanup_at = None
        kill_at = None
        valid = None
        try:
            pidfd, out, err, barrier = spawn(artifactfd)
            fds = [pidfd, out, err, barrier]
            selector.register(pidfd, selectors.EVENT_READ, "parent")
            selector.register(out, selectors.EVENT_READ, "stdout")
            selector.register(err, selectors.EVENT_READ, "stderr")
            if self.interrupted:
                result["primary_failure"] = "reader-interrupted"
            else:
                require(os.write(barrier, b"G") == 1, "child-release-failed")
                released = True
            os.close(barrier)
            fds.remove(barrier)
            while True:
                now = time.monotonic()
                if result["primary_failure"] is None:
                    if self.interrupted:
                        result["primary_failure"] = "reader-interrupted"
                    elif now >= start + CAPTURE_SECONDS:
                        result["primary_failure"] = "capture-deadline"
                if result["primary_failure"] and cleanup_at is None:
                    cleanup_at = now
                    if not result["parent_pidfd_dead"]:
                        self.terminate(pidfd, signal.SIGTERM, result)
                if (
                    cleanup_at is not None
                    and now >= cleanup_at + TERM_SECONDS
                    and kill_at is None
                ):
                    kill_at = now
                    if not result["parent_pidfd_dead"]:
                        self.terminate(pidfd, signal.SIGKILL, result)
                if kill_at is not None and now >= kill_at + KILL_SECONDS:
                    break
                for key, _mask in selector.select(0.02):
                    if key.data == "parent":
                        result["parent_pidfd_dead"] = True
                        status = os.waitid(os.P_PIDFD, pidfd, os.WEXITED | os.WNOHANG)
                        require(status is not None, "child-reap-unverified")
                        result["parent_reaped"] = True
                        result["exit_status"] = (
                            status.si_status
                            if status.si_code == os.CLD_EXITED
                            else -status.si_status
                        )
                        selector.unregister(pidfd)
                        continue
                    try:
                        block = os.read(key.fd, CHUNK)
                    except BlockingIOError:
                        continue
                    except OSError:
                        result["primary_failure"] = (
                            result["primary_failure"] or "channel-read-failed"
                        )
                        selector.unregister(key.fd)
                        continue
                    if not block:
                        eof.add(key.data)
                        selector.unregister(key.fd)
                        continue
                    target = buffers[key.data]
                    remaining = min(
                        CHANNEL_CAP - len(target),
                        TOTAL_CAP - sum(map(len, buffers.values())),
                    )
                    target.extend(block[:remaining])
                    if len(block) > remaining:
                        result["primary_failure"] = (
                            result["primary_failure"] or "capture-overflow"
                        )
                if result["parent_reaped"] and len(eof) == 2:
                    break
            if not result["parent_reaped"]:
                result["cleanup_failure"] = (
                    result["cleanup_failure"] or "child-exit-unverified"
                )
            elif len(eof) != 2:
                result["cleanup_failure"] = (
                    result["cleanup_failure"] or "capture-eof-unverified"
                )
            if (
                result["parent_reaped"]
                and len(eof) == 2
                and result["primary_failure"] is None
            ):
                valid = report(
                    bytes(buffers["stdout"]),
                    bytes(buffers["stderr"]),
                    result["exit_status"],
                )
                result["guard_report"] = valid
                if not valid["ok"]:
                    result["primary_failure"] = "guard-reported-failure"
                    if "cleanup_failure" in valid:
                        result["cleanup_failure"] = "guard-cleanup-unverified"
        except Rejected as error:
            result["primary_failure"] = result["primary_failure"] or str(error)
            if str(error) == "child-pin-failed-unreaped":
                result["cleanup_failure"] = "child-exit-unverified"
            if fds and not result["parent_reaped"]:
                # A failure outside the pump leaves termination unverified. The
                # start barrier closes below; never use a numeric PID fallback.
                result["cleanup_failure"] = "reader-cleanup-unverified"
        except (OSError, ValueError, RuntimeError, KeyboardInterrupt, SystemExit):
            result["primary_failure"] = result["primary_failure"] or "reader-failed"
            result["cleanup_failure"] = "reader-cleanup-unverified"
        finally:
            if fds and not result["parent_reaped"] and kill_at is None:
                self.settle(fds[0], result, cleanup_at)
            selector.close()
            for fd in fds:
                try:
                    os.close(fd)
                except OSError:
                    result["cleanup_failure"] = (
                        result["cleanup_failure"] or "descriptor-close-failed"
                    )
        result["possible_unfinished_manager_operation"] = released and not (
            valid
            and valid.get("userspace_quiesced") is True
            and not result["cleanup_failure"]
        )
        result["ok"] = bool(
            valid
            and valid["ok"]
            and not result["primary_failure"]
            and not result["cleanup_failure"]
        )
        result["channels_eof"] = sorted(eof)
        result["capture_seconds"] = round(time.monotonic() - start, 6)
        for name, data in buffers.items():
            result[name + "_base64"] = base64.b64encode(data).decode("ascii")
        return result

    def settle(self, pidfd, result, cleanup_at):
        # Exceptional pump/setup failure only: do not trust the channels. Keep
        # their read ends open, bound termination, and reap only through pidfd.
        self.terminate(pidfd, signal.SIGTERM, result)
        deadline = (
            time.monotonic() if cleanup_at is None else cleanup_at
        ) + TERM_SECONDS
        killed = False
        while True:
            try:
                status = os.waitid(os.P_PIDFD, pidfd, os.WEXITED | os.WNOHANG)
                if status is not None:
                    result["parent_pidfd_dead"] = True
                    result["parent_reaped"] = True
                    result["exit_status"] = (
                        status.si_status
                        if status.si_code == os.CLD_EXITED
                        else -status.si_status
                    )
                    return
            except (OSError, ValueError):
                result["cleanup_failure"] = (
                    result["cleanup_failure"] or "child-reap-unverified"
                )
                return
            if time.monotonic() >= deadline:
                if killed:
                    result["cleanup_failure"] = (
                        result["cleanup_failure"] or "child-exit-unverified"
                    )
                    return
                self.terminate(pidfd, signal.SIGKILL, result)
                killed = True
                deadline = time.monotonic() + KILL_SECONDS
            time.sleep(0.01)

    def terminate(self, pidfd, signum, result):
        try:
            signal.pidfd_send_signal(pidfd, signum)
            result["signals"].append(signal.Signals(signum).name)
        except ProcessLookupError:
            pass
        except OSError:
            result["cleanup_failure"] = (
                result["cleanup_failure"] or "pidfd-signal-failed"
            )


def evidence_root():
    root = os.open(".", os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW)
    try:
        info = os.fstat(root)
        require(
            info.st_uid == os.getuid() and info.st_mode & 0o022 == 0, "evidence-root"
        )
        return root
    except BaseException:
        os.close(root)
        raise


def save_evidence(result, root):
    """Only after capture: fresh owned directory, anchored exclusive files."""
    data = (
        json.dumps(result, ensure_ascii=True, allow_nan=False, separators=(",", ":"))
        + "\n"
    ).encode("ascii")
    require(len(data) <= EVIDENCE_CAP, "evidence-size")
    directory = None
    record = None
    try:
        info = os.fstat(root)
        require(
            info.st_uid == os.getuid() and info.st_mode & 0o022 == 0, "evidence-root"
        )
        name = "benign-resource-capture-" + os.urandom(16).hex()
        os.mkdir(name, 0o700, dir_fd=root)
        directory = os.open(
            name,
            os.O_RDONLY | os.O_DIRECTORY | os.O_CLOEXEC | os.O_NOFOLLOW,
            dir_fd=root,
        )
        saved = os.fstat(directory)
        require(
            saved.st_uid == os.getuid() and stat.S_IMODE(saved.st_mode) == 0o700,
            "evidence-directory",
        )
        record = os.open(
            "evidence.json",
            os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600,
            dir_fd=directory,
        )
        info = os.fstat(record)
        require(
            stat.S_ISREG(info.st_mode)
            and info.st_uid == os.getuid()
            and info.st_nlink == 1
            and stat.S_IMODE(info.st_mode) == 0o600,
            "evidence-file",
        )
        while data:
            count = os.write(record, data)
            require(count > 0, "evidence-write")
            data = data[count:]
        os.fsync(record)
        require(
            identity(record)
            == (
                os.stat(
                    "evidence.json", dir_fd=directory, follow_symlinks=False
                ).st_dev,
                os.stat(
                    "evidence.json", dir_fd=directory, follow_symlinks=False
                ).st_ino,
            ),
            "evidence-file-drift",
        )
        os.fsync(directory)
        current = os.stat(name, dir_fd=root, follow_symlinks=False)
        require(
            (current.st_dev, current.st_ino) == (saved.st_dev, saved.st_ino),
            "evidence-directory-drift",
        )
        os.fsync(root)
        return name
    finally:
        for fd in (record, directory):
            if fd is not None:
                os.close(fd)


def main():
    require(len(sys.argv) == 1 and os.getuid() != 0, "fixed-reader-entrypoint")
    activation_review_required()
    size_limit = resource.getrlimit(resource.RLIMIT_FSIZE)[0]
    require(
        size_limit == resource.RLIM_INFINITY or size_limit >= EVIDENCE_CAP,
        "reader-fsize-limit",
    )
    root = evidence_root()
    artifactfd = None
    capture = Capture()
    handlers = {}
    try:
        artifactfd = fixed_artifact()
        for signum in (signal.SIGTERM, signal.SIGHUP, signal.SIGINT):
            handlers[signum] = signal.signal(signum, capture.interrupt)
        result = capture.run(artifactfd)
        result["artifact"] = ARTIFACT
        result["artifact_sha256"] = ARTIFACT_SHA256
        save_evidence(result, root)
        return 0 if result["ok"] else 1
    finally:
        for signum, handler in handlers.items():
            signal.signal(signum, handler)
        if artifactfd is not None:
            os.close(artifactfd)
        os.close(root)


if __name__ == "__main__":
    try:
        code = main()
    except (Rejected, OSError, ValueError, KeyboardInterrupt, SystemExit):
        code = 78
    os._exit(code)
