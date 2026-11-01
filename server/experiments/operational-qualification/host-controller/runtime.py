"""Linux operations. Importing this module performs no OS inspection or work."""

import hashlib
import os
import re
import selectors
import signal
import socket
import stat
import struct
import subprocess
import time

from policy import (
    INVENTORY,
    LIMITS,
    MAX_COMMAND,
    MAX_FILE,
    NAMESPACES,
    RETAINED,
    SERVICE_READBACK,
    Rejected,
    cgroup_path,
    cleanup_inventory,
    encode,
    identity,
    require,
    run_name,
    run_path,
    unit,
)

NOFOLLOW = os.O_NOFOLLOW | os.O_CLOEXEC


def file_identity(st):
    return {
        "device": st.st_dev,
        "inode": st.st_ino,
        "mode": st.st_mode,
        "uid": st.st_uid,
    }


def read_fd(fd, maximum):
    value = bytearray()
    for _ in range(32):
        block = os.read(fd, maximum + 1 - len(value))
        value.extend(block)
        require(len(value) <= maximum, "oversized-evidence")
        if not block:
            return bytes(value)
    raise Rejected("read-attempt-limit")


def read_path(path, maximum=MAX_FILE):
    fd = os.open(path, os.O_RDONLY | NOFOLLOW)
    try:
        require(stat.S_ISREG(os.fstat(fd).st_mode), "regular-evidence")
        return read_fd(fd, maximum)
    finally:
        os.close(fd)


def text_path(path, maximum=MAX_FILE):
    try:
        return read_path(path, maximum).decode("ascii")
    except UnicodeError:
        raise Rejected("non-ascii-evidence") from None


def clean_env():
    uid = os.getuid()
    require(uid > 0, "non-root-owner")
    return {
        "LANG": "C",
        "LC_ALL": "C",
        "PATH": "/nonexistent",
        "XDG_RUNTIME_DIR": f"/run/user/{uid}",
        "DBUS_SESSION_BUS_ADDRESS": f"unix:path=/run/user/{uid}/bus",
    }


def pump(process, caps, timeout, hook=None):
    """One deadline and separate hard caps; never communicate()/journal buffering."""
    deadline = time.monotonic() + timeout
    selector = selectors.DefaultSelector()
    buffers = {name: bytearray() for name in caps}
    for name, (fd, _) in caps.items():
        os.set_blocking(fd, False)
        selector.register(fd, selectors.EVENT_READ, name)
    pidfd = os.pidfd_open(process.pid)
    try:
        while selector.get_map():
            remaining = deadline - time.monotonic()
            require(remaining > 0, "pipe-deadline")
            for key, _ in selector.select(min(remaining, 0.05)):
                name = key.data
                cap = caps[name][1]
                data = os.read(key.fd, min(4096, cap + 1 - len(buffers[name])))
                buffers[name].extend(data)
                require(len(buffers[name]) <= cap, "pipe-output-limit")
                if not data:
                    selector.unregister(key.fd)
            if hook is not None:
                hook({name: bytes(value) for name, value in buffers.items()})
        require(time.monotonic() < deadline, "pipe-deadline")
        process.wait(timeout=max(0.001, deadline - time.monotonic()))
        return {name: bytes(value) for name, value in buffers.items()}
    finally:
        selector.close()
        if process.poll() is None:
            signal.pidfd_send_signal(pidfd, signal.SIGKILL)
            try:
                process.wait(timeout=1)
            except subprocess.TimeoutExpired:
                pass  # The independently enforced transient service deadline remains.
        os.close(pidfd)


def command(argv):
    process = subprocess.Popen(
        argv,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=clean_env(),
        close_fds=True,
    )
    try:
        data = pump(
            process,
            {
                "out": (process.stdout.fileno(), MAX_COMMAND),
                "err": (process.stderr.fileno(), MAX_COMMAND),
            },
            3,
        )
        require(process.returncode == 0 and not data["err"], "command-failed")
        return data["out"]
    finally:
        process.stdout.close()
        process.stderr.close()


SHOW = (
    "LoadState",
    "ActiveState",
    "SubState",
    "InvocationID",
    "MainPID",
    "ControlGroup",
    "Result",
    "ExecMainCode",
    "ExecMainStatus",
) + tuple(SERVICE_READBACK)


def manager_transport_required():
    """No legacy client may connect until bounded authenticated transport exists."""
    raise Rejected("authenticated-manager-transport-unimplemented")


def show(paths, nonce):
    manager_transport_required()
    data = command(
        [
            paths["systemctl"],
            "--user",
            "show",
            "--no-pager",
            "--property=" + ",".join(SHOW),
            "--",
            unit(nonce),
        ]
    )
    result = {}
    for line in data.decode("ascii").splitlines():
        key, sep, value = line.partition("=")
        require(sep and key in SHOW and key not in result, "service-properties")
        result[key] = value
    require(result.keys() == set(SHOW), "service-properties")
    return result


def process_identity(pid):
    require(type(pid) is int and pid > 0, "process-id")
    data = text_path(f"/proc/{pid}/stat", 4096)
    end = data.rfind(") ")
    require(end > 0 and data[: data.find(" ")] == str(pid), "process-stat")
    tail = data[end + 2 :].split()
    require(len(tail) >= 20 and tail[19].isdecimal(), "process-start")
    return {
        "pid": pid,
        "start": int(tail[19]),
        "boot": text_path("/proc/sys/kernel/random/boot_id", 64).strip(),
    }


def ns_snapshot(pid):
    result = {}
    for name in NAMESPACES:
        value = os.readlink(f"/proc/{pid}/ns/{name}")
        match = re.fullmatch(name + r":\[([1-9][0-9]*)\]", value)
        require(match is not None, "namespace-link")
        result[name] = int(match[1])
    return result


def cgroup_membership(pid):
    value = text_path(f"/proc/{pid}/cgroup", 1024)
    require(
        value.startswith("0::/") and value.endswith("\n") and value.count("\n") == 1,
        "cgroup-v2",
    )
    return cgroup_path(value[3:-1])


class Cgroup:
    def __init__(self, path):
        self.path = cgroup_path(path)
        self.fd = os.open("/sys/fs/cgroup", os.O_RDONLY | os.O_DIRECTORY | NOFOLLOW)
        try:
            for part in path[1:].split("/"):
                child = os.open(
                    part, os.O_RDONLY | os.O_DIRECTORY | NOFOLLOW, dir_fd=self.fd
                )
                os.close(self.fd)
                self.fd = child
            self.identity = file_identity(os.fstat(self.fd))
        except BaseException:
            os.close(self.fd)
            raise

    def read(self, name):
        require(
            name in LIMITS or name in {"cgroup.procs", "cgroup.events"}, "cgroup-file"
        )
        fd = os.open(name, os.O_RDONLY | NOFOLLOW, dir_fd=self.fd)
        try:
            return read_fd(fd, 4096).decode("ascii")
        finally:
            os.close(fd)

    def limits(self):
        return {name: self.read(name) for name in LIMITS}

    def members(self):
        rows = self.read("cgroup.procs").splitlines()
        require(
            len(rows) <= 16 and all(re.fullmatch(r"[1-9][0-9]*", row) for row in rows),
            "cgroup-members",
        )
        members = [int(row) for row in rows]
        require(len(set(members)) == len(members), "cgroup-members")
        return set(members)

    def close(self):
        os.close(self.fd)


class RunRoot:
    """Anchored exact tree. Evidence is retained; cleanup removes only transients."""

    def __init__(self, uid, nonce, create=False):
        require(uid == os.getuid(), "runtime-actual-owner")
        self.name = run_name(uid, nonce)
        self.path = run_path(uid, nonce)
        self.parent_fds = []
        self.parents = {}
        self.fd = -1
        self.owned = {}
        try:
            # Every existing component is opened without following symlinks.
            # Never create, chmod or substitute a missing runtime directory.
            parts = ("/", "run", "user", str(uid))
            path = ""
            for index, part in enumerate(parts):
                parent = self.parent_fds[-1] if self.parent_fds else None
                fd = os.open(
                    part, os.O_RDONLY | os.O_DIRECTORY | NOFOLLOW, dir_fd=parent
                )
                self.parent_fds.append(fd)
                st = os.fstat(fd)
                private = index == len(parts) - 1
                require(
                    stat.S_ISDIR(st.st_mode)
                    and st.st_uid == (uid if private else 0)
                    and (
                        stat.S_IMODE(st.st_mode) == 0o700
                        if private
                        else not st.st_mode & 0o022
                    ),
                    "runtime-parent-owner-mode",
                )
                path = "/" if index == 0 else path.rstrip("/") + "/" + part
                self.parents[path] = file_identity(st)
            self.parent = self.parent_fds[-1]
            self.check_parents()
            if create:
                os.mkdir(
                    self.name, 0o700, dir_fd=self.parent
                )  # EEXIST is never retried.
            self.fd = os.open(
                self.name, os.O_RDONLY | os.O_DIRECTORY | NOFOLLOW, dir_fd=self.parent
            )
            st = os.fstat(self.fd)
            require(
                stat.S_ISDIR(st.st_mode)
                and st.st_uid == uid
                and stat.S_IMODE(st.st_mode) == 0o700,
                "run-root-owner",
            )
            self.identity = file_identity(st)
            self.check()
        except BaseException:
            self.close()
            raise

    def check_parents(self):
        for index, (path, expected) in enumerate(self.parents.items()):
            parent = self.parent_fds[index - 1] if index else None
            leaf = path.rsplit("/", 1)[-1] if index else "/"
            require(
                file_identity(os.fstat(self.parent_fds[index])) == expected
                and file_identity(os.stat(leaf, dir_fd=parent, follow_symlinks=False))
                == expected,
                "replaced-runtime-parent",
            )

    def check(self):
        self.check_parents()
        require(
            file_identity(os.fstat(self.fd)) == self.identity
            and file_identity(
                os.stat(self.name, dir_fd=self.parent, follow_symlinks=False)
            )
            == self.identity,
            "replaced-root",
        )

    def parent_for(self, name):
        require(name in INVENTORY, "owned-name")
        parts = name.split("/")
        fd = os.dup(self.fd)
        try:
            for part in parts[:-1]:
                child = os.open(
                    part, os.O_RDONLY | os.O_DIRECTORY | NOFOLLOW, dir_fd=fd
                )
                os.close(fd)
                fd = child
            return fd, parts[-1]
        except BaseException:
            os.close(fd)
            raise

    def create_file(self, name, data=b""):
        self.check()
        parent, leaf = self.parent_for(name)
        try:
            fd = os.open(
                leaf,
                os.O_WRONLY | os.O_CREAT | os.O_EXCL | NOFOLLOW,
                0o600,
                dir_fd=parent,
            )
            try:
                self.owned[name] = file_identity(os.fstat(fd))
                require(os.write(fd, data) == len(data), "short-file-write")
                os.fsync(fd)
            finally:
                os.close(fd)
        finally:
            os.close(parent)

    def inventory(self):
        self.check()
        result = {}
        # Only these three directories are traversed; no untrusted recursion.
        for prefix in ("",):
            if prefix:
                try:
                    parent, leaf = self.parent_for(prefix)
                    try:
                        directory = os.open(
                            leaf, os.O_RDONLY | os.O_DIRECTORY | NOFOLLOW, dir_fd=parent
                        )
                    finally:
                        os.close(parent)
                except FileNotFoundError:
                    continue
            else:
                directory = os.dup(self.fd)
            try:
                entries = os.listdir(directory)
                require(len(entries) <= len(INVENTORY), "unexpected-entry")
                for name in entries:
                    relative = prefix + "/" + name if prefix else name
                    require(relative in INVENTORY, "unexpected-entry")
                    result[relative] = file_identity(
                        os.stat(name, dir_fd=directory, follow_symlinks=False)
                    )
            finally:
                os.close(directory)
        return result

    def read(self, name):
        require(name in RETAINED or name == "host-file", "owned-evidence")
        self.check()
        fd = os.open(name, os.O_RDONLY | NOFOLLOW, dir_fd=self.fd)
        try:
            require(stat.S_ISREG(os.fstat(fd).st_mode), "regular-evidence")
            return read_fd(fd, MAX_FILE)
        finally:
            os.close(fd)

    def write(self, name, value, expected):
        require(name in RETAINED, "owned-evidence")
        self.check()
        fd = os.open(name, os.O_WRONLY | NOFOLLOW, dir_fd=self.fd)
        try:
            require(
                file_identity(os.fstat(fd)) == identity(expected), "replaced-evidence"
            )
            require(os.fstat(fd).st_size == 0, "evidence-collision")
            data = encode(value)
            require(os.write(fd, data) == len(data), "short-file-write")
            os.fsync(fd)
        finally:
            os.close(fd)

    def cleanup(self, recorded, partial=False):
        for name in cleanup_inventory(recorded, self.inventory(), partial):
            self.check()
            parent, leaf = self.parent_for(name)
            try:
                require(
                    file_identity(os.stat(leaf, dir_fd=parent, follow_symlinks=False))
                    == recorded[name],
                    "replaced-entry",
                )
                if stat.S_ISDIR(recorded[name]["mode"]):
                    os.rmdir(leaf, dir_fd=parent)
                else:
                    os.unlink(leaf, dir_fd=parent)
            finally:
                os.close(parent)
        os.fsync(self.fd)

    def close(self):
        if self.fd >= 0:
            os.close(self.fd)
            self.fd = -1
        for fd in reversed(self.parent_fds):
            os.close(fd)
        self.parent_fds.clear()


class Sentinels:
    def __init__(self, root, nonce):
        self.root = root
        self.nonce = nonce.encode("ascii")
        self.listeners = []
        try:
            root.create_file("host-file", self.nonce)
            for address in (root.path + "/host.sock", b"\0waddle-host-" + self.nonce):
                sock = socket.socket(
                    socket.AF_UNIX, socket.SOCK_STREAM | socket.SOCK_CLOEXEC
                )
                self.listeners.append(sock)
                sock.settimeout(0.5)
                root.check()
                sock.bind(address)  # EADDRINUSE is a collision; never unlink and retry.
                if isinstance(address, str):
                    root.check()
                    created = os.stat(
                        "host.sock", dir_fd=root.fd, follow_symlinks=False
                    )
                    require(stat.S_ISSOCK(created.st_mode), "created-socket-type")
                    root.owned["host.sock"] = file_identity(created)
                sock.listen(1)
        except BaseException:
            self.close()
            raise

    def positive(self):
        require(self.root.read("host-file") == self.nonce, "file-positive")
        for listener in self.listeners:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
                client.settimeout(0.5)
                self.root.check()
                client.connect(listener.getsockname())
                self.root.check()
                peer = struct.unpack(
                    "3i", client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12)
                )
                require(
                    peer == (os.getpid(), os.getuid(), os.getgid()), "sentinel-peer"
                )
                accepted, _ = listener.accept()
                with accepted:
                    accepted.settimeout(0.5)
                    client.sendall(self.nonce)
                    require(accepted.recv(33) == self.nonce, "socket-positive")
                    accepted.sendall(self.nonce)
                    require(client.recv(33) == self.nonce, "socket-positive")

    def negative_queue(self):
        self.root.check()
        for listener in self.listeners:
            listener.setblocking(False)
            try:
                connection, _ = listener.accept()
            except BlockingIOError:
                pass
            else:
                connection.close()
                raise Rejected("sentinel-contacted")
            finally:
                listener.settimeout(0.5)

    def close(self):
        for sock in self.listeners:
            sock.close()


def pinned(path, digest, static=False):
    require(os.path.isabs(path) and os.path.realpath(path) == path, "artifact-path")
    fd = os.open(path, os.O_RDONLY | NOFOLLOW)
    try:
        st = os.fstat(fd)
        require(stat.S_ISREG(st.st_mode) and not st.st_mode & 0o022, "artifact-mode")
        if path.startswith("/nix/store/"):
            require(not st.st_mode & 0o222, "immutable-store-artifact")
        else:
            require(st.st_uid in (0, os.getuid()), "artifact-owner")
        require(st.st_size <= 32 * 1024 * 1024, "artifact-size")
        data = bytearray()
        for _ in range(513):
            block = os.read(fd, 65536)
            data.extend(block)
            if not block:
                break
        require(hashlib.sha256(data).hexdigest() == digest, "artifact-digest")
        if static:
            require(data[:6] == b"\x7fELF\x02\x01", "static-elf")
            # ELF64 little endian; reject PT_INTERP and executable GNU_STACK.
            offset = int.from_bytes(data[32:40], "little")
            size = int.from_bytes(data[54:56], "little")
            count = int.from_bytes(data[56:58], "little")
            require(
                size == 56 and count <= 32 and offset + count * size <= len(data),
                "elf-headers",
            )
            for index in range(count):
                header = data[offset + index * size : offset + (index + 1) * size]
                kind = int.from_bytes(header[:4], "little")
                flags = int.from_bytes(header[4:8], "little")
                require(
                    kind != 3 and not (kind == 0x6474E551 and flags & 1),
                    "dynamic-or-executable-stack",
                )
        return file_identity(st)
    finally:
        os.close(fd)
