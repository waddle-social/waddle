"""Pure, fail-closed policy for the fixed no-VM host canary."""

import json
import re

NAMESPACES = ("user", "pid", "mnt", "net", "ipc", "uts", "cgroup")
MAX_FILE = 65536
MAX_COMMAND = 8192
MAX_PROBE = 8192
MAX_WITNESS = 512
MAX_INFO = 2048
SERVICE_SECONDS = 30
SANDBOX_SECONDS = 20
PROPERTIES = {
    "Type": "exec",
    "Restart": "no",
    "RemainAfterExit": "yes",
    "KillMode": "control-group",
    "SendSIGKILL": "yes",
    "TimeoutStartSec": "5s",
    "RuntimeMaxSec": "30s",
    "TimeoutStopSec": "2s",
    "CPUAccounting": "yes",
    "CPUQuota": "50%",
    "CPUQuotaPeriodSec": "100ms",
    "MemoryAccounting": "yes",
    "MemoryMax": "134217728",
    "MemorySwapMax": "0",
    "TasksAccounting": "yes",
    "TasksMax": "16",
    "LimitFSIZE": "65536",
    "LimitCORE": "0",
    "LimitNOFILE": "64",
    "NoNewPrivileges": "yes",
    "UMask": "0077",
    "StandardInput": "null",
    "StandardOutput": "null",
    "StandardError": "null",
}
LIMITS = {
    "cpu.max": "50000 100000\n",
    "memory.max": "134217728\n",
    "memory.swap.max": "0\n",
    "pids.max": "16\n",
}
SERVICE_READBACK = {
    "Type": "exec",
    "Restart": "no",
    "RemainAfterExit": "yes",
    "KillMode": "control-group",
    "SendSIGKILL": "yes",
    "TimeoutStartUSec": "5s",
    "RuntimeMaxUSec": "30s",
    "TimeoutStopUSec": "2s",
    "LimitFSIZE": "65536",
    "LimitFSIZESoft": "65536",
    "NoNewPrivileges": "yes",
    "StandardInput": "null",
    "StandardOutput": "null",
    "StandardError": "null",
}
RETAINED = {
    "state.json",
    "launch.json",
    "service.json",
    "result.json",
    "controller.json",
}
TRANSIENT = {"host-file", "host.sock"}
ROOT_TREE = set()
INVENTORY = RETAINED | TRANSIENT | ROOT_TREE


class Rejected(RuntimeError):
    """Errors carry fixed check names, never uncontrolled OS/probe output."""


def require(condition, check):
    if not condition:
        raise Rejected(check)


def token(value):
    require(type(value) is str and re.fullmatch(r"[0-9a-f]{32}", value), "run-token")
    return value


def run_name(uid, nonce):
    require(type(uid) is int and 0 < uid < 2**32 - 1, "non-root-owner")
    return f"waddle-host-{uid}-{token(nonce)}"


def unit(nonce):
    return f"waddle-host-{token(nonce)}.service"


def unique_object(pairs):
    obj = {}
    for key, value in pairs:
        require(key not in obj, "duplicate-json-field")
        obj[key] = value
    return obj


def decode(data, maximum=MAX_FILE):
    require(type(data) is bytes and 0 < len(data) <= maximum, "evidence-size")
    try:
        return json.loads(
            data.decode("ascii"),
            object_pairs_hook=unique_object,
            parse_int=unsigned_integer,
            parse_float=invalid_number,
            parse_constant=invalid_number,
        )
    except (ValueError, UnicodeError, RecursionError):
        raise Rejected("malformed-evidence") from None


def unsigned_integer(value):
    require(
        value.isascii() and value.isdecimal() and len(value) <= 20, "evidence-integer"
    )
    result = int(value)
    require(result < 2**64, "evidence-integer")
    return result


def invalid_number(_value):
    raise Rejected("non-integer-evidence")


def encode(value):
    data = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("ascii")
    require(len(data) <= MAX_FILE, "evidence-size")
    return data


def fields(value, keys):
    require(type(value) is dict and value.keys() == set(keys), "evidence-fields")


def identity(value):
    fields(value, ("device", "inode", "mode", "uid"))
    require(all(type(n) is int and n >= 0 for n in value.values()), "file-identity")
    require(value["inode"] > 0, "file-identity")
    return value


def cleanup_inventory(recorded, current, partial=False):
    """Check the entire tree before deleting anything; absent transients are OK."""
    require(
        recorded.keys() <= INVENTORY if partial else recorded.keys() == INVENTORY,
        "recorded-inventory",
    )
    require(current.keys() <= recorded.keys(), "unexpected-entry")
    require((RETAINED & recorded.keys()) <= current.keys(), "missing-evidence")
    for name, observed in current.items():
        require(identity(recorded[name]) == identity(observed), "replaced-entry")
    return [
        name
        for name in sorted(TRANSIENT | ROOT_TREE, key=lambda x: (-x.count("/"), x))
        if name in current
    ]


def same_service(saved, actual, process=None):
    for key in ("InvocationID", "ControlGroup"):
        require(saved[key] == actual[key] and saved[key], "replaced-service")
    require(re.fullmatch(r"[0-9a-f]{32}", actual["InvocationID"]), "invocation-id")
    if actual["MainPID"] != "0":
        require(saved["MainPID"] == actual["MainPID"], "replaced-pid")
        require(process == saved["process"], "stale-process")


def service_exit_success(actual):
    require(
        actual.get("Result") == "success"
        and actual.get("ExecMainCode") == "1"
        and actual.get("ExecMainStatus") == "0"
        and actual.get("MainPID") == "0",
        "unsuccessful-service-exit",
    )


def service_success(actual):
    service_exit_success(actual)
    require(
        actual.get("ActiveState") == "active" and actual.get("SubState") == "exited",
        "unsuccessful-service-exit",
    )


def cgroup_path(value):
    require(
        type(value) is str and re.fullmatch(r"/[A-Za-z0-9_.@:/\\-]+", value),
        "cgroup-path",
    )
    require(all(p not in ("", ".", "..") for p in value[1:].split("/")), "cgroup-path")
    return value


def limits(actual):
    require(actual == LIMITS, "effective-cgroup-limits")


def service_controls(actual):
    require(
        {key: actual.get(key) for key in SERVICE_READBACK} == SERVICE_READBACK,
        "effective-service-controls",
    )


def namespaces(baseline, observed):
    require(
        baseline.keys() == observed.keys() == set(NAMESPACES), "namespace-inventory"
    )
    for name in NAMESPACES:
        require(type(observed[name]) is int and observed[name] > 0, "namespace-id")
        require(type(baseline[name]) is int and baseline[name] > 0, "baseline-id")
        require(baseline[name] != observed[name], "namespace-separation")


def id_map(data, owner):
    require(data.split() == ["0", str(owner), "1"], "external-id-map")


def mount_records(data):
    require(len(data) <= 32768 and data.endswith("\n"), "mountinfo-size")
    mounts = {}
    ids = set()
    for line in data.splitlines():
        parts = line.split(" ")
        require(len(parts) == 10 and parts[6] == "-", "mountinfo-fields")
        mid, parent, dev, root, target, options, _, fs, _source, _superoptions = parts
        require(mid.isdecimal() and parent.isdecimal() and mid not in ids, "mount-id")
        require(re.fullmatch(r"[0-9]+:[0-9]+", dev), "mount-device")
        require(root.startswith("/") and "\\" not in root, "mount-root")
        require(target not in mounts, "duplicate-mount")
        ids.add(mid)
        mounts[target] = {
            "id": mid,
            "parent": parent,
            "root": root,
            "options": set(options.split(",")),
            "fs": fs,
        }
    return mounts


def mounts(data):
    found = mount_records(data)
    require(found.keys() == {"/", "/proc", "/probe", "/dev/null"}, "mount-inventory")
    # relatime is a kernel default, explicitly permitted; no other extra flags.
    expected = {
        "/": ({"ro", "nosuid", "nodev", "noexec"}, {"tmpfs"}, "/newroot"),
        "/proc": ({"ro", "nosuid", "nodev", "noexec"}, {"proc"}, "/"),
        "/probe": ({"ro", "nosuid", "nodev"}, {"tmpfs", "ext4", "btrfs", "xfs"}, None),
        "/dev/null": ({"rw", "nosuid", "noexec"}, {"tmpfs", "devtmpfs"}, "/null"),
    }
    for target, (options, filesystems, root) in expected.items():
        item = found[target]
        require(item["options"] in (options, options | {"relatime"}), "mount-flags")
        require(item["fs"] in filesystems, "mount-filesystem")
        if root is not None:
            require(item["root"] == root, "mount-source-root")
        if target != "/":
            require(item["parent"] == found["/"]["id"], "mount-parent")
    require(found["/"]["parent"] != found["/"]["id"], "root-mount-parent")


def sandbox_argv(paths, nonce, uid, witness_fd, gate_fd, info_fd):
    """All operands derive from pinned artifacts, owned FDs, and generated IDs."""
    run_name(uid, nonce)
    return [
        paths["bwrap"],
        "--unshare-user",
        "--unshare-pid",
        "--unshare-net",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup",
        "--uid",
        "0",
        "--gid",
        "0",
        "--cap-drop",
        "ALL",
        "--cap-add",
        "CAP_SYS_ADMIN",
        "--cap-add",
        "CAP_SETFCAP",
        "--cap-add",
        "CAP_SETPCAP",
        "--clearenv",
        "--new-session",
        "--die-with-parent",
        "--as-pid-1",
        "--hostname",
        "waddle-canary",
        "--chdir",
        "/",
        "--proc",
        "/proc",
        "--dev-bind",
        "/dev/null",
        "/dev/null",
        "--ro-bind",
        paths["probe"],
        "/probe",
        "--info-fd",
        str(info_fd),
        "--",
        f"/proc/self/fd/{witness_fd}",
        nonce,
        str(uid),
        str(gate_fd),
    ]


def service_argv(paths, nonce):
    return (
        [
            paths["systemd-run"],
            "--user",
            "--quiet",
            "--no-block",
            "--unit=" + unit(nonce),
        ]
        + ["--property=" + key + "=" + value for key, value in PROPERTIES.items()]
        + ["--", paths["bootstrap"], "service", token(nonce)]
    )
