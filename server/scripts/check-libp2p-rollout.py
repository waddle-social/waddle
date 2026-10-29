#!/usr/bin/env python3
"""Exercise baseline/current Kameo actors over loopback TCP and QUIC.

Each peer gets an isolated Cargo workspace and its own dependency resolution,
so old and new libp2p types and Kameo singleton state cannot unify accidentally.
No running Waddle server, production address, database, or credentials are used.
"""
import argparse
import io
import json
import os
from pathlib import Path
import re
import selectors
import shutil
import subprocess
import tarfile
import tempfile


ROOT = Path(__file__).resolve().parents[2]
FIXTURE = Path(__file__).with_name("fixtures") / "libp2p_rollout_peer.rs"
KAMEO_PATH = "server/vendor/kameo"


def resolve_baseline(reference):
    if not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9._/-]*", reference):
        raise ValueError("baseline must be a commit hash or ASCII branch/tag reference")
    try:
        commit = subprocess.check_output(
            ["git", "rev-parse", "--verify", "--end-of-options", reference + "^{commit}"],
            cwd=ROOT, text=True, stderr=subprocess.PIPE,
        ).strip()
    except subprocess.CalledProcessError as error:
        raise ValueError("baseline must resolve to a commit") from error
    if not re.fullmatch(r"(?:[0-9a-f]{40}|[0-9a-f]{64})", commit):
        raise ValueError("Git returned an invalid baseline commit ID")
    return commit


def git_file(revision, path):
    return subprocess.check_output(["git", "show", f"{revision}:{path}", "--"], cwd=ROOT)


def package_version(lockfile, package):
    match = re.search(
        r'\[\[package\]\]\nname = "' + re.escape(package) + r'"\nversion = "([^"]+)"',
        lockfile.decode(),
    )
    if not match:
        raise RuntimeError(f"{package} missing from lockfile")
    return match.group(1)


def prepare_peer(directory, baseline, label):
    directory.mkdir()
    if label == "old":
        lockfile = git_file(baseline, "server/Cargo.lock")
        archive = subprocess.check_output(
            ["git", "archive", baseline, "--", KAMEO_PATH], cwd=ROOT
        )
        with tarfile.open(fileobj=io.BytesIO(archive)) as sources:
            # git archive emits only tracked files under this fixed prefix.
            sources.extractall(directory)
        kameo = directory / KAMEO_PATH
        patch = ""
    else:
        lockfile = (ROOT / "server/Cargo.lock").read_bytes()
        kameo = directory / "kameo"
        shutil.copytree(ROOT / KAMEO_PATH, kameo)
        patch = '[patch.crates-io]\nlibp2p-swarm = { path = ' + json.dumps(str(ROOT / "server/vendor/libp2p-swarm")) + ' }\n'
    version = package_version(lockfile, "libp2p")
    (directory / "Cargo.lock").write_bytes(lockfile)
    manifest = f'''[package]
name = "waddle-libp2p-{label}-probe"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
futures = "0.3"
kameo = {{ path = {json.dumps(str(kameo))}, default-features = false, features = ["macros", "remote"] }}
libp2p = {{ version = "={version}", default-features = false, features = ["tokio", "tcp", "quic", "noise", "yamux", "macros"] }}
serde = {{ version = "1", features = ["derive"] }}
serde_json = "1"
tokio = {{ version = "1", features = ["macros", "rt-multi-thread", "sync", "time"] }}

[profile.dev]
debug = 0
incremental = false

{patch}'''
    (directory / "Cargo.toml").write_text(manifest)
    (directory / "src").mkdir()
    shutil.copyfile(FIXTURE, directory / "src/main.rs")
    return version


def read_message(process):
    with selectors.DefaultSelector() as selector:
        selector.register(process.stdout, selectors.EVENT_READ)
        if not selector.select(timeout=50):
            raise RuntimeError("timed out waiting for loopback peer")
    line = process.stdout.readline()
    if not line:
        raise RuntimeError("loopback peer exited: " + process.stderr.read())
    return json.loads(line)


def run_pair(binaries, transport, dialer):
    peers = {}
    try:
        for label, binary in binaries.items():
            peers[label] = subprocess.Popen(
                [str(binary), label, transport, "dial" if label == dialer else "listen"],
                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                text=True,
            )
        addresses = {label: read_message(peer) for label, peer in peers.items()}
        for label, peer in peers.items():
            other = "new" if label == "old" else "old"
            peer.stdin.write(json.dumps(addresses[other]) + "\n")
            peer.stdin.flush()
        for peer in peers.values():
            result = read_message(peer)
            if not all(result.get(key) for key in ("ok", "registry_lookup", "remote_ask")):
                raise RuntimeError(f"peer failed compatibility check: {result}")
        for peer in peers.values():
            peer.stdin.write("stop\n")
            peer.stdin.flush()
        for peer in peers.values():
            _, errors = peer.communicate(timeout=10)
            if peer.returncode:
                raise RuntimeError(errors)
        print(f"PASS {transport}: {dialer} dialer; actor discovery and remote asks in both directions", flush=True)
    finally:
        for peer in peers.values():
            peer.kill()
            peer.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--baseline", default="origin/main", help="Git revision before the dependency upgrade")
    parser.add_argument("--target-dir", type=Path, help="Optional retained Cargo cache; otherwise removed on completion")
    arguments = parser.parse_args()
    try:
        baseline = resolve_baseline(arguments.baseline)
    except ValueError as error:
        parser.error(str(error))
    with tempfile.TemporaryDirectory(prefix="waddle-libp2p-rollout-") as temporary:
        temporary = Path(temporary)
        target = arguments.target_dir.resolve() if arguments.target_dir else temporary / "target"
        versions = {}
        binaries = {}
        environment = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_BUILD_JOBS="2")
        for label in ("old", "new"):
            directory = temporary / label
            versions[label] = prepare_peer(directory, baseline, label)
            subprocess.run(["cargo", "build", "--manifest-path", str(directory / "Cargo.toml")], cwd=ROOT / "server", env=environment, check=True)
            binaries[label] = target / "debug" / f"waddle-libp2p-{label}-probe"
        if versions["old"] == versions["new"]:
            raise RuntimeError("baseline and current libp2p versions match; this would not test a mixed rollout")
        print(f"Testing libp2p {versions['old']} <-> {versions['new']} with Kameo actor registration/lookup/ask", flush=True)
        for transport in ("tcp", "quic"):
            for dialer in ("old", "new"):
                run_pair(binaries, transport, dialer)


if __name__ == "__main__":
    main()
