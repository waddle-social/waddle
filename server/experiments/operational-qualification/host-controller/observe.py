"""Trusted observer: read kernel state while the static witness waits at its gate."""

import os
import re
import stat

from policy import NAMESPACES, id_map, mounts, namespaces, require
from runtime import (
    cgroup_membership,
    file_identity,
    manager_transport_required,
    ns_snapshot,
    process_identity,
    text_path,
)


def status_fields(data):
    result = {}
    for line in data.splitlines():
        key, sep, value = line.partition(":")
        require(sep and key not in result, "process-status")
        result[key] = value.strip()
    return result


def groups(data):
    values = status_fields(data).get("Groups", "missing").split()
    require(
        len(values) <= 64 and all(re.fullmatch(r"[0-9]+", value) for value in values),
        "supplementary-group-vector",
    )
    numbers = [int(value) for value in values]
    require(all(value < 2**32 - 1 for value in numbers), "supplementary-group-id")
    return numbers


def verify_status(data, uid, gid, expected_groups, require_empty_bounding=True):
    status = status_fields(data)
    require(status.get("Uid", "").split() == [str(uid)] * 4, "external-process-uid")
    require(status.get("Gid", "").split() == [str(gid)] * 4, "external-process-gid")
    require(groups(data) == expected_groups, "supplementary-groups")
    caps = ("CapInh", "CapPrm", "CapEff", "CapAmb")
    if require_empty_bounding:
        caps += ("CapBnd",)
    for name in caps:
        require(re.fullmatch("0{16}", status.get(name, "")), "process-capabilities")
    require(status.get("NoNewPrivs") == "1", "no-new-privileges")


def baseline(cgroup, artifacts):
    # No authenticated external manager record exists yet. Do not substitute a
    # client package image or /proc PPid for endpoint authentication.
    manager_transport_required()
    pid = os.getpid()
    service_status = text_path("/proc/self/status", 16384)
    parent = status_fields(service_status).get("PPid", "")
    require(re.fullmatch(r"[1-9][0-9]*", parent), "service-manager-parent")
    manager = process_identity(int(parent))
    manager_status = text_path(f"/proc/{parent}/status", 16384)
    expected_groups = groups(manager_status)
    require(
        status_fields(manager_status).get("Uid", "").split() == [str(os.getuid())] * 4,
        "manager-owner",
    )
    verify_status(service_status, os.getuid(), os.getgid(), expected_groups, False)
    require(process_identity(int(parent)) == manager, "manager-process-replaced")
    require(cgroup_membership(pid) == cgroup.path, "service-cgroup-membership")
    require(cgroup.members() == {pid}, "baseline-cgroup-processes")
    count = status_fields(service_status).get("Seccomp_filters", "")
    require(
        count == "0" and status_fields(service_status).get("Seccomp") == "0",
        "unknown-inherited-seccomp",
    )
    return {
        "process": process_identity(pid),
        "namespaces": ns_snapshot(pid),
        "manager": manager,
        "groups": expected_groups,
        "seccomp_filters": int(count),
        "uid": os.getuid(),
        "gid": os.getgid(),
        "cgroup": cgroup.path,
        "cgroup_identity": cgroup.identity,
        "limits": cgroup.limits(),
    }


def verify_environment(data, image):
    # /proc/PID/environ exposes the initial exec environment, not clearenv().
    # Pinned bwrap unconditionally adds PWD after --clearenv and --chdir /.
    require(image in {"witness", "probe"}, "fixed-executable")
    require(data == ("PWD=/\0" if image == "witness" else ""), "image-environment")


def verify_fsize(data):
    rows = [
        line.split() for line in data.splitlines() if line.startswith("Max file size")
    ]
    require(
        rows == [["Max", "file", "size", "65536", "65536", "bytes"]], "process-fsize"
    )


def fd_snapshot(pid, gate_fd, expected):
    base = f"/proc/{pid}/fd"
    require(
        set(os.listdir(base)) == {"0", "1", "2", str(gate_fd)}, "descriptor-inventory"
    )
    found = {}
    for number in (0, 1, 2, gate_fd):
        path = base + "/" + str(number)
        st = os.stat(path)
        fields = dict(
            line.split(":", 1)
            for line in text_path(f"/proc/{pid}/fdinfo/{number}", 1024).splitlines()
        )
        require(
            "flags" in fields and re.fullmatch(r"\s*[0-7]+", fields["flags"]),
            "descriptor-flags",
        )
        flags = int(fields["flags"], 8)
        require(not flags & os.O_PATH, "path-descriptor")
        require(
            not flags & ~(os.O_ACCMODE | os.O_NONBLOCK | os.O_CLOEXEC | 0o100000),
            "unexpected-descriptor-flags",
        )
        require(
            flags & os.O_ACCMODE == (os.O_WRONLY if number in (1, 2) else os.O_RDONLY),
            "descriptor-access",
        )
        if number == 0:
            require(
                stat.S_ISCHR(st.st_mode)
                and os.major(st.st_rdev) == 1
                and os.minor(st.st_rdev) == 3,
                "null-descriptor",
            )
        else:
            require(
                stat.S_ISFIFO(st.st_mode)
                and os.readlink(path) == f"pipe:[{st.st_ino}]",
                "anonymous-pipe",
            )
            require(st.st_ino == expected[number], "pipe-identity")
        found[str(number)] = st.st_ino
        if number in (2, gate_fd):
            require(flags & os.O_NONBLOCK, "nonblocking-barrier-descriptor")
    require(len({found["1"], found["2"], found[str(gate_fd)]}) == 3, "distinct-pipes")
    return found


def image_mappings(data, image_identity):
    rows = data.splitlines()
    require(0 < len(rows) <= 64, "mapping-count")
    image_seen = False
    for row in rows:
        parts = row.split(None, 5)
        require(len(parts) >= 5, "mapping-fields")
        _address, permissions, _offset, device, inode = parts[:5]
        name = parts[5] if len(parts) == 6 else ""
        require(
            inode.isdecimal() and re.fullmatch(r"[r-][w-][x-][ps]", permissions),
            "mapping-fields",
        )
        if int(inode):
            require(re.fullmatch(r"[0-9a-f]+:[0-9a-f]+", device), "mapping-device")
            major, minor = (int(value, 16) for value in device.split(":"))
            require(
                int(inode) == image_identity["inode"]
                and os.makedev(major, minor) == image_identity["device"],
                "foreign-image-mapping",
            )
            image_seen = True
        else:
            require(
                name
                in {
                    "",
                    "[heap]",
                    "[stack]",
                    "[vdso]",
                    "[vvar]",
                    "[vvar_vclock]",
                    "[vsyscall]",
                },
                "anonymous-mapping-name",
            )
            require(
                "x" not in permissions
                or (name in {"[vdso]", "[vsyscall]"} and "w" not in permissions),
                "anonymous-executable-mapping",
            )
    require(image_seen, "missing-image-mapping")


def sandbox(
    pid,
    pidfd,
    trusted,
    cgroup,
    artifacts,
    gate_fd,
    pipe_inodes,
    filter_layers,
    image="witness",
):
    # The open pidfd and start/boot tuple prevent accepting a recycled PID.
    require(pidfd >= 0, "sandbox-pidfd")
    before = process_identity(pid)
    require(image in {"witness", "probe"}, "fixed-executable")
    require(
        file_identity(os.stat(f"/proc/{pid}/exe")) == artifacts[image],
        "observed-executable",
    )
    image_mappings(text_path(f"/proc/{pid}/maps", 32768), artifacts[image])
    namespace = ns_snapshot(pid)
    namespaces(trusted["namespaces"], namespace)
    id_map(text_path(f"/proc/{pid}/uid_map", 256), trusted["uid"])
    id_map(text_path(f"/proc/{pid}/gid_map", 256), trusted["gid"])
    process_status = text_path(f"/proc/{pid}/status", 16384)
    verify_status(process_status, trusted["uid"], trusted["gid"], trusted["groups"])
    status = status_fields(process_status)
    require(status.get("Threads") == "1", "single-threaded-barrier")
    require(
        status.get("Seccomp") == "2"
        and status.get("Seccomp_filters")
        == str(trusted["seccomp_filters"] + filter_layers),
        "seccomp-layer-observation",
    )
    verify_fsize(text_path(f"/proc/{pid}/limits", 4096))
    verify_environment(text_path(f"/proc/{pid}/environ", 1024), image)
    require(
        cgroup_membership(pid) == cgroup.path and pid in cgroup.members(),
        "sandbox-cgroup-membership",
    )
    mounts(text_path(f"/proc/{pid}/mountinfo", 32768))
    root = f"/proc/{pid}/root"
    require(set(os.listdir(root)) == {"dev", "proc", "probe"}, "root-file-inventory")
    require(set(os.listdir(root + "/dev")) == {"null"}, "device-inventory")
    for name in ("dev", "proc"):
        require(
            stat.S_ISDIR(os.lstat(root + "/" + name).st_mode), "root-directory-type"
        )
    require(
        file_identity(os.stat(root + "/probe")) == artifacts["probe"],
        "probe-mount-source",
    )
    device = os.stat(root + "/dev/null")
    require(
        stat.S_ISCHR(device.st_mode)
        and os.major(device.st_rdev) == 1
        and os.minor(device.st_rdev) == 3,
        "mounted-null",
    )
    descriptors = fd_snapshot(pid, gate_fd, pipe_inodes)
    require(process_identity(pid) == before, "sandbox-process-replaced")
    require(
        file_identity(os.stat(f"/proc/{pid}/exe")) == artifacts[image]
        and ns_snapshot(pid) == namespace,
        "barrier-state-changed",
    )
    return {
        "origin": "trusted_service_kernel_observer",
        "image": image,
        "image_identity": artifacts[image],
        "process": before,
        "namespaces": namespace,
        "descriptors_at_gate": descriptors,
        "mounts": "exact_four_and_authenticated_probe_inode",
        "root_inventory": ["dev/null", "proc", "probe"],
        "uid_gid": "zero_maps_to_service_owner",
        "capabilities": "all_zero",
        "groups": trusted["groups"],
        "seccomp_filter_layers_added": filter_layers,
        "fsize": 65536,
        "cgroup": cgroup.path,
    }


def match_probe(payload, actual):
    observed = payload["observed"]
    require(
        all(value["state"] == "observed" for value in observed.values()),
        "probe-incomplete",
    )
    require(
        observed["status"]["supplementary_groups"] == len(actual["groups"]),
        "probe-group-count",
    )
    require(
        observed["namespaces"]["inodes"]
        == [actual["namespaces"][name] for name in NAMESPACES],
        "probe-namespace-mismatch",
    )
    descriptors = observed["descriptors"]
    require(
        descriptors["stdout_inode"] == actual["descriptors_at_gate"]["1"]
        and descriptors["stderr_inode"] == actual["descriptors_at_gate"]["2"],
        "probe-pipe-mismatch",
    )
