"""Qualify a Namespace Cargo volume on the server's default-feature test suite."""

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time
import xml.etree.ElementTree as ET
from pathlib import Path

PROBE = "cargo_volume_changed_server_source"
COMPILED = re.compile(r"^\s*Compiling (\S+) v[0-9]")
COMMAND = [
    "cargo",
    "nextest",
    "run",
    "--workspace",
    "--all-targets",
    "--locked",
    "--profile",
    "ci",
]


def run_tests(root, phase):
    server = root / "source/server"
    report = server / "target/nextest/ci/junit.xml"
    report.unlink(missing_ok=True)
    if phase == "changed":
        # A new test in the largest crate proves that this source edit
        # invalidates a populated target directory and reaches execution.
        with (server / "crates/waddle-server/src/lib.rs").open("a") as source:
            source.write(f"\n#[cfg(test)]\n#[test]\nfn {PROBE}() {{}}\n")

    compiled = []
    compile_seconds = None
    started = time.monotonic()
    env = dict(
        os.environ,
        CARGO_TARGET_DIR=str(root / "target"),
        CARGO_BUILD_JOBS="1",
        CARGO_INCREMENTAL="0",
        RUST_MIN_STACK="8388608",
        CARGO_TERM_COLOR="never",
    )
    with subprocess.Popen(
        COMMAND,
        cwd=server,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    ) as process:
        for line in process.stdout:
            print(line, end="", flush=True)
            if line.lstrip().startswith("Finished `test` profile"):
                compile_seconds = time.monotonic() - started
            if match := COMPILED.match(line):
                compiled.append(match.group(1))
        if process.wait() != 0:
            raise SystemExit(f"{phase}: server tests failed")

    elapsed = time.monotonic() - started
    if compile_seconds is None:
        raise SystemExit(f"{phase}: missing Cargo build timing")
    if not report.is_file():
        raise SystemExit(f"{phase}: missing nextest report")
    suites = ET.parse(report).getroot()
    cases = [
        (case.attrib["classname"], case.attrib["name"])
        for case in suites.iter("testcase")
        if case.find("skipped") is None
    ]
    if not cases:
        raise SystemExit(f"{phase}: no tests executed")

    free_gib = shutil.disk_usage(root).free / (1024**3)
    with open(os.environ["GITHUB_STEP_SUMMARY"], "a") as summary:
        summary.write(
            f"**{phase}** on {os.environ['RUNNER_NAME']}: {len(compiled)} compilation messages, "
            f"{compile_seconds:.1f} seconds to compile, {len(cases)} tests in {suites.attrib['time']} seconds, "
            f"{elapsed:.1f} seconds total. Free volume space: {free_gib:.1f} GiB.\n\n"
        )

    if phase == "seed" and "waddle-server" not in compiled:
        raise SystemExit("cold seed did not compile waddle-server")
    if phase == "warm" and compiled:
        raise SystemExit(f"warm run recompiled {len(compiled)} targets")
    if phase == "changed":
        if "waddle-server" not in compiled or ("waddle-server", PROBE) not in cases:
            raise SystemExit(
                "source change did not rebuild waddle-server and execute the additional test"
            )
        cases.remove(("waddle-server", PROBE))
    return hashlib.sha256(json.dumps(sorted(cases)).encode()).hexdigest()


def main():
    (phase,) = sys.argv[1:]
    if phase not in ("seed", "restore"):
        raise SystemExit("expected seed or restore")

    root = Path.home() / ".cache/waddle-cargo-trial"
    if root.stat().st_dev != Path("/cache").stat().st_dev:
        raise SystemExit("Cargo trial directory is not on the Namespace cache volume")
    free_gib = shutil.disk_usage(root).free / (1024**3)
    print(f"{free_gib:.1f} GiB available on the Cargo volume", flush=True)
    marker = root / "seed.json"
    revision = os.environ["GITHUB_SHA"]
    runner = os.environ["RUNNER_NAME"]
    if phase == "seed":
        (root / "source").mkdir()
        if (root / "target").exists() or marker.exists():
            raise SystemExit("seed requires an empty trial directory")
    else:
        seed = json.loads(marker.read_text())
        if seed["revision"] != revision or seed["runner"] == runner:
            raise SystemExit(
                "restore requires the exact revision seeded on a different runner"
            )

    # Keep Cargo's absolute source paths stable and retain input mtimes only
    # when rsync verifies identical content. Remove deleted sources and outputs.
    subprocess.run(
        [
            "rsync",
            "-rlpc",
            "--delete",
            "--exclude=.git",
            os.environ["GITHUB_WORKSPACE"] + "/",
            str(root / "source") + "/",
        ],
        check=True,
    )

    if phase == "seed":
        inventory = run_tests(root, "seed")
        with open(os.environ["GITHUB_OUTPUT"], "a") as output:
            output.write(f"inventory={inventory}\n")
        marker.write_text(json.dumps({"revision": revision, "runner": runner}))
    else:
        expected = os.environ["SEED_INVENTORY"]
        for trial in ("warm", "changed"):
            if run_tests(root, trial) != expected:
                raise SystemExit(f"{trial}: executed test inventory differs from seed")


if __name__ == "__main__":
    main()
