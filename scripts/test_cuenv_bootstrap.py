#!/usr/bin/env python3
"""Execute the generated cuenv bootstrap with isolated, hash-checked fixtures."""

import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
DIGESTS = {
    "cuenv-darwin-arm64": "8ffe172f09390308b90378f64723b07a2689761351c37affec308aa656e0aaba",
    "cuenv-linux-arm64": "0c6dd349b14a11d3740b07aecef566150ddc808dd95fdcc127feaa58df605519",
    "cuenv-linux-x64": "5f9a363851b6031972c09e55e177753a7474de073d98ede5ecf2a2a4133197fa",
}


def generated_bootstraps():
    source = """
import {readdirSync, readFileSync} from 'node:fs';
const result = [];
for (const file of readdirSync('.github/workflows').filter(file => /\\.ya?ml$/.test(file))) {
  const workflow = Bun.YAML.parse(readFileSync('.github/workflows/' + file, 'utf8'));
  for (const [id, job] of Object.entries(workflow.jobs ?? {})) {
    for (const step of job.steps ?? []) {
      if (step.name === 'Setup cuenv (release)' || step.run?.includes('/cuenv/cuenv/releases/')) {
        result.push({file, id, name: step.name, run: step.run});
      }
    }
  }
}
console.log(JSON.stringify(result));
"""
    output = subprocess.run(["bun", "-e", source], cwd=ROOT, check=True, capture_output=True, text=True)
    return json.loads(output.stdout)


class BootstrapContracts(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.steps = generated_bootstraps()
        cls.script = next(step["run"] for step in cls.steps if step["file"] == "waddle-chat-pullrequest.yml")

    def test_all_release_bootstraps_are_exact_and_pinned(self):
        self.assertGreater(len(self.steps), 20)
        self.assertTrue(any(step["file"] == "waddle-integration.yml" for step in self.steps))
        for step in self.steps:
            with self.subTest(file=step["file"], job=step["id"]):
                self.assertEqual(step["name"], "Setup cuenv (release)")
                self.assertEqual(step["run"], self.script)
        self.assertEqual(dict(re.findall(r'cuenv_asset="([^"]+)"\s+cuenv_sha256="([0-9a-f]+)"', self.script)), DIGESTS)
        self.assertIn('cuenv_version="0.63.2"', self.script)
        self.assertNotIn("latest", self.script)
        self.assertNotIn("/usr/local", self.script)
        plan = subprocess.run(["bun", "-e", "console.log(Bun.YAML.parse(await Bun.file('.github/workflows/waddle-integration.yml').text()).jobs.plan.steps.map(step=>step.run??'').join('\\n'))"], cwd=ROOT, check=True, capture_output=True, text=True)
        self.assertIn("python3 scripts/test_cuenv_bootstrap.py", plan.stdout)

    def exercise(self, *, system="Linux", arch="x86_64", altered=False, version="0.63.2", missing=False, bootstrap_version=None, sync_exit=0):
        # Replace pinned digests only in this isolated executable fixture. The
        # separate full-graph assertion above verifies every real release pin.
        with tempfile.TemporaryDirectory(prefix="waddle-cuenv-bootstrap-") as temporary:
            root = Path(temporary)
            tools = root / "tools"; tools.mkdir()
            runner = root / "runner"; runner.mkdir()
            github_path = root / "github-path"; github_path.touch()
            trace = root / "executed"
            url_trace = root / "download-url"
            payload = ("#!" + shutil.which("sh") + "\n"
                       'printf "%s\\n" "$1" >> "$TEST_EXECUTION_LOG"\n'
                       'case "$1" in\n'
                       '  --version) printf "cuenv %s\\n" "$TEST_PAYLOAD_VERSION" ;;\n'
                       '  sync) test "$*" = "sync vcs -p ."; exit "$TEST_SYNC_EXIT" ;;\n'
                       '  *) exit 9 ;;\n'
                       'esac\n').encode()
            digest = hashlib.sha256(payload).hexdigest()
            download = root / "payload"
            download.write_bytes(payload + (b"\n# altered bytes\n" if altered else b""))
            script = self.script
            for pinned in DIGESTS.values():
                script = script.replace(pinned, "" if missing else digest)
            if bootstrap_version is not None:
                script = script.replace('cuenv_version="0.63.2"', 'cuenv_version="' + bootstrap_version + '"')
            (tools / "uname").write_text("#!/bin/sh\ncase \"$1\" in -s) echo \"$TEST_SYSTEM\" ;; -m) echo \"$TEST_ARCH\" ;; *) exit 9 ;; esac\n")
            (tools / "curl").write_text("#!" + shutil.which("python3") + "\nimport os, pathlib, shutil, sys\nargs=sys.argv[1:]\nassert args[:5]==['--proto','=https','--proto-redir','=https','-fsSL']\nassert args[5]=='-o' and len(args)==8\npathlib.Path(os.environ['TEST_DOWNLOAD_URL']).write_text(args[7])\nshutil.copyfile(os.environ['TEST_DOWNLOAD_PAYLOAD'],args[6])\n")
            # Simulate Darwin's shasum CLI while using the real host SHA-256
            # implementation. Native Darwin/arm64 binary execution is a hosted check.
            sha256sum = shutil.which("sha256sum")
            self.assertIsNotNone(sha256sum, "bootstrap tests require the Linux plan runner's sha256sum")
            (tools / "shasum").write_text("#!/bin/sh\ntest \"$*\" = '-a 256 -c -' || exit 9\nexec " + shlex.quote(sha256sum) + " -c -\n")
            for tool in tools.iterdir():
                tool.chmod(0o755)
            environment = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"], RUNNER_TEMP=str(runner), GITHUB_PATH=str(github_path), TEST_SYSTEM=system, TEST_ARCH=arch, TEST_DOWNLOAD_PAYLOAD=str(download), TEST_DOWNLOAD_URL=str(url_trace), TEST_EXECUTION_LOG=str(trace), TEST_PAYLOAD_VERSION=version, TEST_SYNC_EXIT=str(sync_exit))
            result = subprocess.run(["bash", "-c", script], cwd=root, env=environment, capture_output=True, text=True)
            executions = trace.read_text().splitlines() if trace.exists() else []
            installed = github_path.read_text().splitlines()
            if altered or missing or bootstrap_version is not None or system not in ("Linux", "Darwin") or arch == "unsupported":
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(executions, [], "unverified bytes must never execute")
                self.assertEqual(installed, [])
                self.assertEqual(list(runner.iterdir()), [])
            elif version != "0.63.2":
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("version differs", result.stderr)
                self.assertEqual(executions, ["--version"])
                self.assertEqual(installed, [], "wrong version must not install or enter GITHUB_PATH")
                self.assertEqual(list(runner.iterdir()), [])
            else:
                self.assertEqual(result.returncode, sync_exit, result.stdout + result.stderr)
                self.assertEqual(executions, ["--version", "sync"])
                self.assertEqual(len(installed), 1)
                binary = Path(installed[0]) / "cuenv"
                binary.relative_to(runner)
                self.assertEqual(binary.read_bytes(), payload)
                self.assertEqual(binary.stat().st_mode & 0o777, 0o755)
            if url_trace.exists():
                expected_asset = "cuenv-darwin-arm64" if system == "Darwin" else ("cuenv-linux-x64" if arch in ("x86_64", "amd64") else "cuenv-linux-arm64")
                self.assertEqual(url_trace.read_text(), "https://github.com/cuenv/cuenv/releases/download/0.63.2/" + expected_asset)

    def test_valid_digest_version_and_platform_mapping(self):
        for system, arch in (("Linux", "x86_64"), ("Linux", "amd64"), ("Linux", "arm64"), ("Linux", "aarch64"), ("Darwin", "arm64"), ("Darwin", "aarch64")):
            with self.subTest(system=system, arch=arch):
                self.exercise(system=system, arch=arch)

    def test_altered_bytes_and_missing_digest_never_execute(self):
        for system, arch in (("Linux", "x86_64"), ("Linux", "aarch64"), ("Darwin", "arm64")):
            with self.subTest(system=system, arch=arch):
                self.exercise(system=system, arch=arch, altered=True)
                self.exercise(system=system, arch=arch, missing=True)

    def test_correct_digest_wrong_version_is_rejected_before_install(self):
        self.exercise(version="0.57.3")

    def test_wrong_pinned_version_and_unsupported_platform_fail(self):
        self.exercise(bootstrap_version="0.63.3")
        self.exercise(system="Windows", arch="arm64")
        self.exercise(system="Linux", arch="unsupported")
        self.exercise(system="Darwin", arch="unsupported")

    def test_sync_failure_propagates(self):
        self.exercise(sync_exit=7)


if __name__ == "__main__":
    unittest.main()
