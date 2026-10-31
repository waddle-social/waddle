"""Executable controller; direct Python invocation is not the supported entrypoint."""

import importlib.util
import os
import resource
import secrets
import signal
import subprocess
import sys
import time
from pathlib import Path

# -I -S excludes site/user imports. This one reviewed source directory is explicit.
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
from observe import baseline, match_probe, sandbox
from policy import (
    INVENTORY,
    MAX_INFO,
    MAX_PROBE,
    MAX_WITNESS,
    SANDBOX_SECONDS,
    SERVICE_SECONDS,
    Rejected,
    decode,
    fields,
    limits,
    parent_identities,
    require,
    same_service,
    sandbox_argv,
    service_argv,
    service_controls,
    service_exit_success,
    service_success,
    token,
)
from runtime import (
    Cgroup,
    RunRoot,
    Sentinels,
    command,
    manager_transport_required,
    pinned,
    process_identity,
    pump,
    read_path,
    show,
)

WITNESS_RESULT = b"READY\nSENTINELS file=ENOENT path=ENOENT abstract=ECONNREFUSED local=ok\nINSPECTOR_READY\n"
PROBE_READY = WITNESS_RESULT + b"PROBE_READY\n"


def configuration():
    require(str(HERE).startswith("/nix/store/"), "locked-runtime-package-required")
    config = decode(read_path(str(HERE / "config.json")))
    fields(config, {"paths", "digests", "sources"})
    required = {
        "python",
        "systemd-run",
        "systemctl",
        "bwrap",
        "bootstrap",
        "witness",
        "probe",
        "decoder",
    }
    require(
        config["paths"].keys() == config["digests"].keys() == required,
        "configuration-artifacts",
    )
    artifacts = {}
    for name in required:
        path = config["paths"][name]
        if name in {"python", "systemd-run", "systemctl", "bwrap"}:
            require(path.startswith("/nix/store/"), "immutable-runtime-closure")
        artifacts[name] = pinned(
            path,
            config["digests"][name],
            static=name in {"bootstrap", "witness", "probe"},
        )
    artifacts["digests"] = config["digests"]
    artifacts["sources"] = config["sources"]
    expected_sources = {
        "controller.py",
        "runtime.py",
        "policy.py",
        "observe.py",
        "bootstrap.c",
        "witness.c",
        "filter.c",
        "filter.h",
        "sentinel_paths.h",
    }
    require(config["sources"].keys() == expected_sources, "configuration-sources")
    for name, digest in config["sources"].items():
        pinned(str(HERE / name), digest)
    spec = importlib.util.spec_from_file_location(
        "canary_probe_frame", config["paths"]["decoder"]
    )
    require(spec is not None and spec.loader is not None, "probe-decoder")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return config["paths"], artifacts, module.decode_frame


def state_for(root, nonce):
    state = decode(root.read("state.json"))
    fields(state, {"version", "nonce", "uid", "gid", "parents", "root", "inventory"})
    require(
        type(state["version"]) is int
        and state["version"] == 3
        and state["nonce"] == nonce
        and state["uid"] == os.getuid()
        and state["gid"] == os.getgid(),
        "run-owner",
    )
    parent_identities(state["parents"], root.parents)
    require(
        state["root"] == root.identity and state["inventory"].keys() == INVENTORY,
        "run-identity",
    )
    actual = root.inventory()
    require(actual == state["inventory"], "run-inventory")
    return state


def launch_sandbox(paths, artifacts, nonce, trusted, cgroup, decoder):
    witness = os.open(paths["witness"], os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    null = os.open("/dev/null", os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW)
    gate_read, gate_write = os.pipe2(os.O_CLOEXEC)
    info_read, info_write = os.pipe2(os.O_CLOEXEC)
    process = None
    child_pidfd = None
    observed = None
    inspector_observed = None
    probe_observed = None
    try:
        argv = sandbox_argv(paths, nonce, os.getuid(), witness, gate_read, info_write)
        process = subprocess.Popen(
            argv,
            stdin=null,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={},
            close_fds=True,
            pass_fds=(witness, gate_read, info_write),
        )
        os.close(info_write)
        info_write = -1
        # Gate fd number in the child remains valid after our local read-end closes.
        pipe_inodes = {
            1: os.fstat(process.stdout.fileno()).st_ino,
            2: os.fstat(process.stderr.fileno()).st_ino,
            3: os.fstat(gate_read).st_ino,
        }

        def inspect(data):
            nonlocal \
                observed, \
                inspector_observed, \
                probe_observed, \
                child_pidfd, \
                gate_write
            if probe_observed is not None:
                return
            require(not data["out"], "premature-probe-output")
            if inspector_observed is not None:
                if data["err"] != PROBE_READY:
                    require(
                        PROBE_READY.startswith(data["err"]), "invalid-probe-progress"
                    )
                    return
                probe_observed = sandbox(
                    observed["process"]["pid"],
                    child_pidfd,
                    trusted,
                    cgroup,
                    artifacts,
                    3,
                    pipe_inodes,
                    2,
                    "probe",
                )
                require(
                    probe_observed["process"] == observed["process"]
                    and probe_observed["namespaces"] == observed["namespaces"],
                    "post-exec-process-change",
                )
                require(os.write(gate_write, b"I") == 1, "probe-gate-release")
                os.close(gate_write)
                gate_write = -1
                return
            if observed is not None:
                if data["err"] != WITNESS_RESULT:
                    require(
                        WITNESS_RESULT.startswith(data["err"]),
                        "invalid-witness-progress",
                    )
                    return
                inspector_observed = sandbox(
                    observed["process"]["pid"],
                    child_pidfd,
                    trusted,
                    cgroup,
                    artifacts,
                    3,
                    pipe_inodes,
                    2,
                )
                require(
                    inspector_observed["process"] == observed["process"]
                    and inspector_observed["namespaces"] == observed["namespaces"],
                    "barrier-process-change",
                )
                require(os.write(gate_write, b"P") == 1, "inspector-gate-release")
                return
            if not data["err"].startswith(b"READY\n"):
                return
            require(data["err"] == b"READY\n", "premature-witness-output")
            try:
                info = decode(data["info"], MAX_INFO)
            except Rejected:
                return  # Incomplete info cannot release the gate; deadline still applies.
            allowed = {"child-pid"} | {
                name + "-namespace"
                for name in ("cgroup", "ipc", "mnt", "net", "pid", "uts")
            }
            require(
                type(info) is dict and info.keys() <= allowed and "child-pid" in info,
                "bwrap-info",
            )
            pid = info["child-pid"]
            require(type(pid) is int and pid > 0, "sandbox-pid")
            require(
                cgroup.members() == {os.getpid(), process.pid, pid},
                "sandbox-cgroup-processes",
            )
            child_pidfd = os.pidfd_open(pid)
            observed = sandbox(
                pid, child_pidfd, trusted, cgroup, artifacts, 3, pipe_inodes, 1
            )
            limits(cgroup.limits())
            require(os.write(gate_write, b"G") == 1, "gate-release")

        output = pump(
            process,
            {
                "out": (process.stdout.fileno(), MAX_PROBE),
                "err": (process.stderr.fileno(), MAX_WITNESS),
                "info": (info_read, MAX_INFO),
            },
            SANDBOX_SECONDS,
            inspect,
        )
        require(probe_observed is not None, "missing-trusted-observation")
        require(
            process.returncode == 2 and output["err"] == PROBE_READY, "sandbox-result"
        )
        try:
            payload = decoder(output["out"])
        except (ValueError, KeyError, TypeError):
            raise Rejected("invalid-probe-frame") from None
        match_probe(payload, probe_observed)
        require(cgroup.members() == {os.getpid()}, "remaining-sandbox-processes")
        limits(cgroup.limits())
        return {
            "trusted_witness_observation": observed,
            "trusted_inspector_observation": inspector_observed,
            "trusted_post_exec_observation": probe_observed,
            "untrusted_probe_output": payload,
            "witness": "pinned_static_sentinel_checks_observed",
            "pipe_bytes": {key: len(value) for key, value in output.items()},
        }
    finally:
        if child_pidfd is not None:
            if process is not None and process.poll() is None:
                try:
                    signal.pidfd_send_signal(child_pidfd, signal.SIGKILL)
                except ProcessLookupError:
                    pass
            os.close(child_pidfd)
        for fd in (witness, null, gate_read, gate_write, info_read, info_write):
            if fd >= 0:
                os.close(fd)
        if process is not None:
            if process.poll() is None:
                # This is our unreaped direct child, not a name/PID lookup. A
                # pre-pump error must not leave a launcher running.
                process.kill()
                try:
                    process.wait(timeout=1)
                except subprocess.TimeoutExpired:
                    pass  # The service's independent deadline remains enforced.
            process.stdout.close()
            process.stderr.close()


def service(paths, artifacts, decoder, nonce):
    manager_transport_required()
    root = RunRoot(os.getuid(), nonce)
    group = None
    state = None
    try:
        state = state_for(root, nonce)
        require(
            resource.getrlimit(resource.RLIMIT_FSIZE) == (65536, 65536), "service-fsize"
        )
        actual = show(paths, nonce)
        service_controls(actual)
        require(
            actual["LoadState"] == "loaded"
            and actual["ActiveState"] in {"activating", "active"},
            "service-state",
        )
        require(
            actual["InvocationID"] == os.environ.get("WADDLE_INVOCATION_ID")
            and actual["MainPID"] == str(os.getpid()),
            "service-invocation",
        )
        group = Cgroup(actual["ControlGroup"])
        limits(group.limits())
        trusted = baseline(group, artifacts)
        record = {
            key: actual[key] for key in ("InvocationID", "MainPID", "ControlGroup")
        }
        record["process"] = trusted["process"]
        record["cgroup_identity"] = group.identity
        root.write("service.json", record, state["inventory"]["service.json"])
        evidence = launch_sandbox(paths, artifacts, nonce, trusted, group, decoder)
        root.write(
            "result.json",
            {
                "version": 1,
                "state": "observed",
                "nonce": nonce,
                "invocation": actual["InvocationID"],
                "baseline": trusted,
                "artifact_sha256": artifacts["digests"],
                "source_sha256": artifacts["sources"],
                "sandbox": evidence,
                "host_confinement": "pending_host_positive_and_cleanup_checks",
                "qemu_guest_isolation": "unverified",
            },
            state["inventory"]["result.json"],
        )
    except (OSError, Rejected, ValueError, KeyError, TypeError):
        # Fixed failure record; never include raw launcher output, paths or secrets.
        if state is not None:
            try:
                root.write(
                    "result.json",
                    {
                        "version": 1,
                        "state": "failed",
                        "nonce": nonce,
                        "host_confinement": "unverified",
                        "qemu_guest_isolation": "unverified",
                    },
                    state["inventory"]["result.json"],
                )
            except (OSError, Rejected):
                pass  # Missing/torn evidence also fails closed at controller.
        raise
    finally:
        if group is not None:
            group.close()
        root.close()


def stop_owned(paths, nonce, record, successful=False):
    actual = show(paths, nonce)
    if successful:
        service_success(actual)
    if actual["LoadState"] == "not-found":
        # Successful transient units may be garbage-collected after stop. Never
        # signal a PID or unit on this path; require the recorded cgroup absent.
        try:
            absent = Cgroup(record["ControlGroup"])
        except FileNotFoundError:
            return
        else:
            absent.close()
            raise Rejected("orphaned-cgroup")
    live = (
        process_identity(int(actual["MainPID"])) if actual["MainPID"] != "0" else None
    )
    same_service(record, actual, live)
    group = None
    try:
        try:
            group = Cgroup(record["ControlGroup"])
        except FileNotFoundError:
            require(
                actual["MainPID"] == "0"
                and (
                    actual["ActiveState"] in {"inactive", "failed"}
                    or actual["SubState"] == "exited"
                ),
                "missing-live-cgroup",
            )
        if group is not None:
            require(group.identity == record["cgroup_identity"], "replaced-cgroup")
        if actual["ActiveState"] not in {"inactive", "failed"} or (
            group is not None and group.members()
        ):
            # Identity is re-read immediately before this name-based systemd API.
            # A hostile same-UID manager client is explicitly outside the model.
            latest = show(paths, nonce)
            if successful:
                service_success(latest)
            current = (
                process_identity(int(latest["MainPID"]))
                if latest["MainPID"] != "0"
                else None
            )
            same_service(record, latest, current)
            command([paths["systemctl"], "--user", "stop", "--", unit_name(nonce)])
        final = show(paths, nonce)
        if final["LoadState"] != "not-found":
            same_service(record, final)
            if successful:
                service_exit_success(final)
            require(
                final["MainPID"] == "0"
                and final["ActiveState"] in {"inactive", "failed"},
                "service-not-stopped",
            )
        if group is not None:
            try:
                require(not group.members(), "cgroup-not-empty")
            except FileNotFoundError:
                pass  # Open, identity-checked removed cgroup; kernel removed its files.
    finally:
        if group is not None:
            group.close()


def unit_name(nonce):
    from policy import unit

    return unit(nonce)


def cleanup(paths, nonce):
    root = RunRoot(os.getuid(), nonce)
    try:
        state = decode(root.read("state.json"))
        fields(
            state, {"version", "nonce", "uid", "gid", "parents", "root", "inventory"}
        )
        require(
            state["version"] == 3
            and state["nonce"] == nonce
            and state["root"] == root.identity
            and state["uid"] == os.getuid()
            and state["gid"] == os.getgid(),
            "cleanup-run-identity",
        )
        parent_identities(state["parents"], root.parents)
        # Sentinels are independently owned files. Their anchored cleanup must
        # not depend on whether a service record was ever completely written.
        root.cleanup(state["inventory"])
        launch = root.read("launch.json")
        service_record = root.read("service.json")
        if not launch:
            require(not service_record, "service-without-launch-intent")
            return {"files": "removed", "service": "not_attempted"}
        # A torn/unknown launch outcome never authorizes a name-based stop.
        intent = decode(launch)
        fields(intent, {"nonce", "state"})
        require(intent == {"nonce": nonce, "state": "attempted"}, "launch-intent")
        if not service_record:
            raise Rejected("service-identity-unavailable-files-cleaned")
        record = decode(service_record)
        stop_owned(paths, nonce, record)
        return {"files": "removed", "service": "stopped"}
    finally:
        root.close()


def run(paths):
    manager_transport_required()
    nonce = secrets.token_hex(16)
    root = RunRoot(os.getuid(), nonce, create=True)
    sentinels = None
    record = None
    inventory = None
    try:
        for name in (
            "state.json",
            "launch.json",
            "service.json",
            "result.json",
            "controller.json",
        ):
            root.create_file(name)
        sentinels = Sentinels(root, nonce)
        inventory = root.inventory()
        require(
            inventory.keys() == INVENTORY and inventory == root.owned,
            "initial-inventory",
        )
        state = {
            "version": 3,
            "nonce": nonce,
            "uid": os.getuid(),
            "gid": os.getgid(),
            "parents": root.parents,
            "root": root.identity,
            "inventory": inventory,
        }
        root.write("state.json", state, inventory["state.json"])
        require(show(paths, nonce)["LoadState"] == "not-found", "unit-collision")
        sentinels.positive()
        # Persist before sending. An interrupted/unknown result is never retried.
        root.write(
            "launch.json",
            {"nonce": nonce, "state": "attempted"},
            inventory["launch.json"],
        )
        command(
            service_argv(paths, nonce)
        )  # StartTransientUnit uses fail-on-existing semantics.
        deadline = time.monotonic() + SERVICE_SECONDS + 4
        while time.monotonic() < deadline:
            if record is None and root.read("service.json"):
                record = decode(root.read("service.json"))
            actual = show(paths, nonce)
            if actual["MainPID"] == "0" and (
                actual["ActiveState"] in {"inactive", "failed"}
                or actual["SubState"] == "exited"
            ):
                break
            time.sleep(0.05)
        else:
            raise Rejected("controller-runtime-deadline")
        require(record is not None, "missing-service-identity")
        same_service(record, actual)
        service_success(actual)
        result = decode(root.read("result.json"))
        require(
            result.get("state") == "observed"
            and result.get("nonce") == nonce
            and result.get("invocation") == record["InvocationID"],
            "missing-run-evidence",
        )
        sentinels.negative_queue()
        sentinels.positive()
        stop_owned(paths, nonce, record, True)
        sentinels.close()
        root.cleanup(inventory)
        root.cleanup(inventory)  # Repeat against the same anchored identity/inventory.
        # Bounded summary goes to the operator; detailed bounded evidence stays in root.
        summary = {
            "state": "observed",
            "run": nonce,
            "invocation": record["InvocationID"],
            "host_positive_controls": "observed_before_and_after",
            "cleanup": "observed_twice",
            "evidence": root.path,
            "qemu_guest_isolation": "unverified",
        }
        root.write("controller.json", summary, inventory["controller.json"])
        return summary
    finally:
        if sentinels is not None:
            sentinels.close()
        try:
            if record is not None:
                stop_owned(paths, nonce, record)
        finally:
            try:
                # Only identities captured at creation, including failed setup
                # prefixes. Never infer ownership from a fresh directory listing.
                root.cleanup(root.owned, partial=True)
            finally:
                root.close()


def main():
    require(
        os.environ.get("WADDLE_STATIC_BOOTSTRAP") == "1", "static-bootstrap-required"
    )
    require(os.getuid() > 0 and os.geteuid() == os.getuid(), "non-root-owner")
    paths, artifacts, decoder = configuration()
    if sys.argv[1:] == ["run"]:
        from policy import encode

        summary = encode(run(paths)) + b"\n"
        os.set_blocking(1, False)
        require(os.write(1, summary) == len(summary), "controller-summary-output")
    elif len(sys.argv) == 3 and sys.argv[1] == "service":
        service(paths, artifacts, decoder, token(sys.argv[2]))
    elif len(sys.argv) == 3 and sys.argv[1] == "cleanup":
        cleanup(paths, token(sys.argv[2]))
    else:
        raise Rejected("fixed-command-required")


if __name__ == "__main__":
    try:
        main()
    except (
        OSError,
        Rejected,
        ValueError,
        KeyError,
        TypeError,
        subprocess.TimeoutExpired,
    ):
        os.write(2, b"host canary failed closed; host and guest isolation unverified\n")
        sys.exit(2)
