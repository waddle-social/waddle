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
            for filename in ("android-ci-gradle.sh", "android-ci-build.sh"):
                helper = REPO_ROOT / "scripts" / filename
                shutil.copy2(helper, scripts / helper.name)
            record = root / "gradle-argv.jsonl"
            gradlew = android / "gradlew"
            gradlew.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                "with open(os.environ['WADDLE_TEST_GRADLE_RECORD'], 'a') as output:\n"
                "    output.write(json.dumps(sys.argv[1:]) + '\\n')\n"
                "if int(os.environ['WADDLE_TEST_GRADLE_EXIT_CODE']):\n"
                "    print('Gradle diagnostic fixture', file=sys.stderr)\n"
                "sys.exit(int(os.environ['WADDLE_TEST_GRADLE_EXIT_CODE']))\n"
            )
            gradlew.chmod(0o755)
            # The GMD entrypoint probes Linux KVM and memory before Gradle.
            # Stub just those probes; every actual shell/helper still executes.
            for name_to_stub in ("ls", "free"):
                stub = bin_dir / name_to_stub
                stub.write_text("#!/bin/sh\nexit 0\n")
                stub.chmod(0o755)
            git = bin_dir / "git"
            git.write_text(
                "#!/bin/sh\n"
                "case \"$1\" in\n"
                "  show) printf '%s\\n' 1730000000 ;;\n"
                "  rev-parse) printf '%s\\n' abc12345 ;;\n"
                "  *) exit 1 ;;\n"
                "esac\n"
            )
            git.chmod(0o755)
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
        self.assertLessEqual(POLICY, set(args), f"Missing bounded CI flags: {POLICY - set(args)}")
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
                self.assertLessEqual(required_args, set(calls[0]), f"Lost checks: {required_args - set(calls[0])}")

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

    def test_pr_apk_can_upgrade_older_rolling_preview(self):
        result, calls = self.run_entrypoint("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"-PversionCode={1730000000 // 60}", calls[0])
        self.assertIn("-PversionName=pr-abc12345", calls[0])

    def test_gradle_error_is_visible_when_ci_only_streams_stdout(self):
        result, _ = self.run_entrypoint("build", exit_code=42)
        self.assertEqual(result.returncode, 42)
        self.assertIn("Gradle diagnostic fixture", result.stdout)
        self.assertNotIn("Gradle diagnostic fixture", result.stderr)

    def test_direct_host_builds_relocate_read_only_flutter_sdk_caches(self):
        result, calls = self.run_entrypoint("build")
        self.assertEqual(result.returncode, 0, result.stderr)
        for prefix, suffix in (("--project-cache-dir=", "/cache"), ("-Pkotlin.project.persistent.dir=", "/kotlin")):
            values = [arg.removeprefix(prefix) for arg in calls[0] if arg.startswith(prefix)]
            self.assertEqual(len(values), 1)
            self.assertTrue(Path(values[0]).is_absolute())
            self.assertTrue(values[0].endswith("/.gradle/waddle-ci" + suffix))
            self.assertNotIn("/nix/store/", values[0])

    def test_apk_upload_finalizers_only_emit_for_build_pipeline(self):
        workflow = (REPO_ROOT / ".github/workflows/waddle-android-pullrequest.yml").read_text()
        pipeline = workflow.index("Run pipeline: pullRequest")
        self.assertIn("--dart-define=WADDLE_PREVIEW_DIR=build/design-previews", self.tasks["flutterCheck"]["args"][-1])
        for outcome, condition in (("success", "success()"), ("failure", "failure()")):
            label = f"Upload Android APK ({outcome})"
            start = workflow.index(f"    - name: {label}")
            end = workflow.find("\n    - name:", start + 1)
            step = workflow[start:end if end != -1 else None]
            self.assertGreater(start, pipeline)
            self.assertIn(f"if: {condition}", step)
            self.assertIn("uses: actions/upload-artifact@", step)
            self.assertIn("name: waddle-android-apk", step)
            self.assertIn("if-no-files-found: error", step)
            self.assertIn("path: apps/android/app/build/outputs/apk/foss/debug/app-foss-debug.apk", step)
            self.assertIn("retention-days: 7", step)
            preview = workflow.index(f"    - name: Upload Flutter design previews ({outcome})")
            preview_end = workflow.find("\n    - name:", preview + 1)
            preview_step = workflow[preview:preview_end if preview_end != -1 else None]
            self.assertGreater(preview, start)
            self.assertIn(f"if: {condition}", preview_step)
            self.assertIn("name: waddle-android-design-previews", preview_step)
            self.assertIn("path: apps/android/flutter/build/design-previews/", preview_step)
            summary = workflow.index(f"    - name: Link Android APK download ({outcome})")
            self.assertGreater(summary, start)
        self.assertNotIn("continue-on-error: true", workflow)
        for filename in ("waddle-android-default.yml", "waddle-android-devicetests.yml"):
            other = (REPO_ROOT / ".github/workflows" / filename).read_text()
            self.assertNotIn("Upload Android APK", other)
            self.assertNotIn("Link Android APK download", other)

    def test_apk_summary_links_exact_run_and_fails_when_apk_missing(self):
        with tempfile.TemporaryDirectory(prefix="waddle-android-apk-summary-") as directory:
            root = Path(directory)
            summary = root / "summary.md"
            summary.write_text("Existing check results\n")
            env = {
                **os.environ,
                "GITHUB_SERVER_URL": "https://github.com",
                "GITHUB_REPOSITORY": "waddle-social/waddle",
                "GITHUB_RUN_ID": "12345",
                "GITHUB_STEP_SUMMARY": str(summary),
            }
            script = REPO_ROOT / "scripts/android-apk-summary.sh"
            missing = subprocess.run(["bash", str(script)], cwd=root, env=env, capture_output=True, text=True)
            self.assertNotEqual(missing.returncode, 0)
            self.assertIn("no download is available", summary.read_text())
            apk = root / "apps/android/app/build/outputs/apk/foss/debug/app-foss-debug.apk"
            apk.parent.mkdir(parents=True)
            apk.write_bytes(b"apk-fixture")
            previews = root / "apps/android/flutter/build/design-previews"
            previews.mkdir(parents=True)
            (previews / "home.png").write_bytes(b"preview-fixture")
            produced = subprocess.run(["bash", str(script)], cwd=root, env=env, capture_output=True, text=True)
            self.assertEqual(produced.returncode, 0, produced.stderr)
            self.assertIn("https://github.com/waddle-social/waddle/actions/runs/12345#artifacts", summary.read_text())
            self.assertIn("does not mean every check passed", summary.read_text())
            self.assertIn("waddle-android-design-previews", summary.read_text())
            self.assertTrue(summary.read_text().startswith("Existing check results\n"))

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

    def test_generated_workflows_bound_parent_evaluator_before_bootstrap(self):
        workflows = (
            "waddle-android-default.yml",
            "waddle-android-pullrequest.yml",
            "waddle-android-devicetests.yml",
        )
        for workflow in workflows:
            with self.subTest(workflow=workflow):
                lines = (REPO_ROOT / ".github/workflows" / workflow).read_text().splitlines()
                # This deliberately reads the generated emitter's fixed step
                # indentation, not arbitrary YAML. Only this one step's run
                # body may execute; installers and publishing never execute.
                steps = []
                for index, line in enumerate(lines):
                    if line.startswith("    - name: "):
                        name = line.removeprefix("    - name: ")
                        if name.startswith("'") and name.endswith("'"):
                            name = name[1:-1].replace("''", "'")
                        steps.append((index, name))
                names = [name for _, name in steps]
                bootstrap_name = "Bound CUE evaluator memory"
                self.assertIn(bootstrap_name, names)
                self.assertEqual(names.count(bootstrap_name), 1)
                bootstrap_position = names.index(bootstrap_name)
                self.assertLess(bootstrap_position, names.index("Install Determinate Nix"))
                self.assertLess(bootstrap_position, names.index("Setup cuenv (release)"))
                pipeline_positions = [
                    index for index, name in enumerate(names) if name.startswith("Run pipeline:")
                ]
                self.assertTrue(pipeline_positions, "Missing generated pipeline step")
                for position in pipeline_positions:
                    self.assertLess(bootstrap_position, position)

                start = steps[bootstrap_position][0] + 1
                end = steps[bootstrap_position + 1][0]
                run_lines = lines[start:end]
                run_position = next(
                    (index for index, line in enumerate(run_lines) if line.startswith("      run: ")),
                    None,
                )
                self.assertIsNotNone(run_position, "Bootstrap must have a generated run script")
                run_value = run_lines[run_position].removeprefix("      run: ")
                if run_value in ("|-", "|"):
                    body = []
                    for line in run_lines[run_position + 1 :]:
                        if line and not line.startswith("        "):
                            break
                        body.append(line[8:] if line else "")
                    script = "\n".join(body)
                else:
                    # The generator may emit a single shell line as a plain
                    # YAML scalar. Quoted scalars are intentionally unsupported.
                    self.assertFalse(run_value.startswith(("'", '"')))
                    script = run_value
                self.assertTrue(script.strip(), "Bootstrap script must not be empty")

                with tempfile.TemporaryDirectory(prefix="waddle-android-bootstrap-contract-") as directory:
                    github_env = Path(directory) / "github-env"
                    github_env.write_text("EXISTING=kept\n")
                    result = subprocess.run(
                        ["bash", "-e", "-c", script],
                        cwd=directory,
                        env={**os.environ, "GITHUB_ENV": str(github_env)},
                        capture_output=True,
                        text=True,
                        timeout=10,
                    )
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(github_env.read_text(), "EXISTING=kept\nGOGC=100\n")


if __name__ == "__main__":
    unittest.main()
