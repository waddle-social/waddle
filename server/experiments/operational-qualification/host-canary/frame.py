"""Pure decoder for bounded, untrusted probe frames; never establishes isolation."""

import json

FRAME_BYTES = 8192
MAX_U64 = 2**64 - 1
PREFIX = b"WADDLE_HOST_PROBE_V1 "
REQUESTED = {
    "namespaces": ["user", "pid", "mnt", "net", "ipc", "uts", "cgroup"],
    "capabilities": "none",
    "no_new_privileges": True,
    "uid_gid_map": "nested_zero_to_parent_zero",
    "supplementary_groups": "externally_verified_vector",
    "mount_profile": "probe_v1",
    "descriptors": "null_pipe_pipe",
    "limit_fsize": 65536,
}
UNAVAILABLE = [
    "trusted_service_baseline",
    "namespace_comparison",
    "external_group_identity",
    "mount_source_identity",
    "runtime_artifact_closure",
    "device_inventory",
    "loader_environment",
    "further_userns_disabled",
    "effective_cgroup_limits",
    "cgroup_process_membership",
    "sentinel_controls",
    "unit_identity",
    "cleanup",
    "controller_deadlines",
    "journal_bounds",
]
FIXED = {
    "schema_version": 1,
    "component": "static_self_inspection",
    "evidence_origin": "untrusted_probe_output",
    "controller": "unavailable",
    "host_confinement": "unverified",
    "qemu_guest_isolation": "unverified",
    "requested": REQUESTED,
    "unavailable": UNAVAILABLE,
}
FIELDS = {
    "status": {"uid", "gid", "caps", "no_new_privileges", "supplementary_groups"},
    "uid_map": {"inside", "outside", "length"},
    "gid_map": {"inside", "outside", "length"},
    "namespaces": {"inodes"},
    "mounts": {"count"},
    "descriptors": {
        "count",
        "stdin_device_major",
        "stdin_device_minor",
        "stdout_inode",
        "stderr_inode",
    },
    "cgroup_namespace_membership": set(),
    "limit_fsize": {"soft", "hard"},
}


class InvalidFrame(ValueError):
    """Intentionally contains no input, parser excerpts, paths or raw evidence."""


def _require(condition):
    if not condition:
        raise InvalidFrame("invalid probe frame")


def _object(pairs):
    result = {}
    for key, value in pairs:
        _require(key not in result)
        result[key] = value
    return result


def _integer(text):
    _require(0 < len(text) <= 20 and text.isascii() and text.isdecimal())
    value = int(text)
    _require(value <= MAX_U64)
    return value


def _not_integer(_text):
    raise InvalidFrame("invalid probe frame")


def _record(value, fields):
    _require(type(value) is dict and value.keys() == fields)


def _unsigned(value):
    _require(type(value) is int and 0 <= value <= MAX_U64)


def _same_typed(value, expected):
    """Python's True == 1 must not turn a type mismatch into acceptance."""
    _require(type(value) is type(expected))
    if type(expected) is dict:
        _record(value, expected.keys())
        for key, child in expected.items():
            _same_typed(value[key], child)
    elif type(expected) is list:
        _require(len(value) == len(expected))
        for child, pattern in zip(value, expected):
            _same_typed(child, pattern)
    else:
        _require(value == expected)


def _check_observation(name, value):
    _record(value, FIELDS[name] | {"state"})
    _require(value["state"] in ("unavailable", "observed", "failed"))
    arrays = {"uid": 4, "gid": 4, "caps": 5, "inodes": 7}
    for key in FIELDS[name]:
        if key in arrays:
            _require(type(value[key]) is list and len(value[key]) == arrays[key])
            for number in value[key]:
                _unsigned(number)
        else:
            _unsigned(value[key])
    if value["state"] != "observed":
        return
    if name == "status":
        _require(value["uid"] == value["gid"] == [0] * 4)
        _require(value["caps"] == [0] * 5)
        _require(
            value["no_new_privileges"] == 1 and value["supplementary_groups"] <= 64
        )
    elif name in ("uid_map", "gid_map"):
        _require(value["inside"] == 0 and value["outside"] == 0)
        _require(value["length"] == 1)
    elif name == "namespaces":
        _require(all(value["inodes"]))
    elif name == "mounts":
        _require(value["count"] == 4)
    elif name == "descriptors":
        _require(value["count"] == 3)
        _require(value["stdin_device_major"] == 1 and value["stdin_device_minor"] == 3)
        _require(value["stdout_inode"] > 0 and value["stderr_inode"] > 0)
        _require(value["stdout_inode"] != value["stderr_inode"])
    elif name == "limit_fsize":
        _require(value["soft"] == value["hard"] == 65536)


def decode_frame(data):
    """Validate one complete in-memory frame. Caller must bound/time stream reads.

    An accepted frame is an untrusted observation record, never a canary pass.
    Failed and unavailable records remain separate; no defaults are supplied.
    """
    _require(type(data) is bytes and 0 < len(data) <= FRAME_BYTES)
    _require(data.startswith(PREFIX))
    header_end = data.find(b"\n", len(PREFIX), 64)
    _require(header_end != -1)
    length_text = data[len(PREFIX) : header_end]
    _require(length_text.isdigit() and not length_text.startswith(b"0"))
    length = int(length_text)
    _require(header_end + 1 + length + 1 == len(data) and data[-1:] == b"\n")
    try:
        payload = json.loads(
            data[header_end + 1 : -1].decode("ascii"),
            object_pairs_hook=_object,
            parse_int=_integer,
            parse_float=_not_integer,
            parse_constant=_not_integer,
        )
    except (ValueError, UnicodeError, RecursionError):
        raise InvalidFrame("invalid probe frame") from None
    _record(payload, FIXED.keys() | {"observed"})
    for key, expected in FIXED.items():
        _same_typed(payload[key], expected)
    observed = payload["observed"]
    _record(observed, FIELDS.keys())
    for name, value in observed.items():
        _check_observation(name, value)
    return payload
