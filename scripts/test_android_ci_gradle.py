#!/usr/bin/env python3
"""Exercise evaluated Android CI commands without Gradle, an SDK, or builds."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


REPO_ROOT = Path(__file__).resolve().parents[1]
HELPER_INPUT = "../../scripts/android-ci-gradle.sh"
POLICY = {
    "--no-daemon",
    "--no-parallel",
    "--max-workers=2",
    "-Pkotlin.compiler.execution.strategy=in-process",
    "-Dorg.gradle.jvmargs=-Xmx5g -XX:+UseParallelGC",
}
REQUIRED_ARGS = {
    "lint": {
        "detekt",
        ":app:lintFossDebug",
        ":app:lintPlayDebug",
        ":app:verifyFossRuntimeClasspath",
        ":app:verifyPlayRuntimeClasspath",
    },
    "test": {
        ":core:client:testDebugUnitTest",
        ":app:testFossDebugUnitTest",
        ":app:testPlayDebugUnitTest",
        "-Proborazzi.test.verify=true",
    },
    "build": {
        ":app:assembleFossDebug",
        ":app:assemblePlayDebug",
        ":app:minifyPlayReleaseWithR8",
        "-PWADDLE_FIREBASE_PROJECT_ID=ci-placeholder",
        "-PWADDLE_FIREBASE_APP_ID=1:0:android:0",
        "-PWADDLE_FIREBASE_API_KEY=ci-placeholder",
        "-PWADDLE_FIREBASE_SENDER_ID=0",
    },
}


class AndroidCiGradleTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Omitting the task name lists evaluated CUE values. It neither runs task
        # dependencies nor resolves the publisher's environment passthrough.
        result = subprocess.run(
            [
                os.environ.get("CUENV_BIN", "cuenv"),
                "task",
                "--path",
                "apps/android",
                "--output",
                "json",
            ],
            cwd=REPO_ROOT,
            capture_output=True,
            text=True,
            check=True,
            timeout=60,
        )
        cls.tasks = {task["name"]: task["node"] for task in json.loads(result.stdout)}

    def run_entrypoint(self, name, exit_code=0):
        with tempfile.TemporaryDirectory(prefix="waddle-android-ci-contract-") as directory:
            root = Path(directory)
            android = root / "apps/android"
            scripts = root / "scripts"
            bin_dir = root / "bin"
            android.mkdir(parents=True)
            scripts.mkdir()
            bin_dir.mkdir()
            helper = REPO_ROOT / "scripts/android-ci-gradle.sh"
            if helper.exists():
                shutil.copy2(helper, scripts / helper.name)
            record = root / "gradle-argv.jsonl"
            gradlew = android / "gradlew"
            gradlew.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "with open(os.environ['WADDLE_TEST_GRADLE_RECORD'], 'a') as output:\n"
                "    output.write(json.dumps(sys.argv[1:]) + '\\n')\n"
                "sys.exit(int(os.environ['WADDLE_TEST_GRADLE_EXIT_CODE']))\n"
            )
            gradlew.chmod(0o755)
            # The GMD entrypoint probes Linux KVM and memory before Gradle.
            # Stub just those probes; every actual shell/helper still executes.
            for name_to_stub in ("ls", "free"):
                stub = bin_dir / name_to_stub
                stub.write_text("#!/bin/sh\nexit 0\n")
                stub.chmod(0o755)
            task = self.tasks[name]
            env = {
                **os.environ,
                "PATH": str(bin_dir) + os.pathsep + os.environ["PATH"],
                "WADDLE_TEST_GRADLE_RECORD": str(record),
                "WADDLE_TEST_GRADLE_EXIT_CODE": str(exit_code),
            }
            result = subprocess.run(
                [task["command"], *task["args"]],
                cwd=android,
                env=env,
                capture_output=True,
                text=True,
                timeout=10,
            )
            calls = (
                [json.loads(line) for line in record.read_text().splitlines()]
                if record.exists()
                else []
            )
            return result, calls

    def assert_policy(self, args):
        self.assertTrue(POLICY <= set(args), f"Missing bounded CI flags: {POLICY - set(args)}")
        # A later conflicting flag could override an earlier correct one.
        self.assertNotIn("--parallel", args)
        self.assertEqual([arg for arg in args if arg.startswith("--max-workers")], ["--max-workers=2"])
        self.assertEqual(
            [arg for arg in args if arg.startswith("-Dorg.gradle.jvmargs=")],
            ["-Dorg.gradle.jvmargs=-Xmx5g -XX:+UseParallelGC"],
        )
        self.assertEqual(
            [arg for arg in args if arg.startswith("-Pkotlin.compiler.execution.strategy=")],
            ["-Pkotlin.compiler.execution.strategy=in-process"],
        )

    def test_ordinary_ci_entrypoints_bound_memory_and_preserve_checks(self):
        for name, required_args in REQUIRED_ARGS.items():
            with self.subTest(task=name):
                result, calls = self.run_entrypoint(name)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(len(calls), 1, calls)
                self.assert_policy(calls[0])
                self.assertTrue(required_args <= set(calls[0]), f"Lost checks: {required_args - set(calls[0])}")

    def test_gmd_builds_apks_before_starting_device_with_bounded_memory(self):
        result, calls = self.run_entrypoint("gmdCheck")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(calls), 2, calls)
        for args in calls:
            self.assert_policy(args)
            self.assertIn("--no-configuration-cache", args)
        self.assertEqual(
            [arg for arg in calls[0] if arg.startswith(":")],
            [":app:assembleFossDebug", ":app:assembleFossDebugAndroidTest"],
        )
        self.assertEqual(
            [arg for arg in calls[1] if arg.startswith(":")],
            [":app:atdApi34FossDebugAndroidTest"],
        )

    def test_gradle_failure_status_is_preserved_and_gmd_stops_before_device(self):
        for name in (*REQUIRED_ARGS, "gmdCheck"):
            with self.subTest(task=name):
                result, calls = self.run_entrypoint(name, exit_code=42)
                self.assertEqual(result.returncode, 42, result.stderr)
                self.assertEqual(len(calls), 1, calls)

    def test_policy_and_entrypoint_changes_select_every_gradle_task(self):
        for name in (*REQUIRED_ARGS, "gmdCheck", "publishApk"):
            with self.subTest(task=name):
                inputs = self.tasks[name]["inputs"]
                self.assertIn("env.cue", inputs)
                self.assertIn(HELPER_INPUT, inputs)

    def test_publisher_uses_shared_policy_without_executing_release_actions(self):
        task = self.tasks["publishApk"]
        self.assertEqual(task["command"], "bash")
        self.assertEqual(task["args"][0], "-c")
        script = task["args"][1]
        self.assertIn("android-ci-gradle.sh", script)
        self.assertIn(":app:assembleFossPreview", script)
        self.assertNotIn("./gradlew", script)
        result = subprocess.run(
            ["bash", "-n", "-c", script], capture_output=True, text=True, check=False
        )
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == "__main__":
    unittest.main()
