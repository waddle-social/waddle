import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


SCRIPT = Path(__file__).with_name("build-nextest-archive.sh")
ARCHIVE = "/nix/store/00000000000000000000000000000000-test-archive"
DEPENDENCY = "/nix/store/11111111111111111111111111111111-dependency"
SHARDS = [f"/nix/store/{str(index) * 32}-test-archive-shard{index}" for index in range(2, 6)]


class ArchiveCacheHookTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.hook_log = self.root / "hook.jsonl"
        self.build_log = self.root / "build.json"
        # Spaces and shell metacharacters must remain literal in the generated
        # wrapper, including when Nix dispatches the hook without arguments.
        original = self.root / "original hook ' $ literal"
        original.write_text(
            f"#!{sys.executable}\n"
            "import json, os, sys\n"
            f"with open({str(self.hook_log)!r}, 'a') as log:\n"
            "    log.write(json.dumps({'paths': os.environ['OUT_PATHS'], "
            "'drv': os.environ['DRV_PATH'], 'args': sys.argv[1:]}) + '\\n')\n"
            "sys.exit(int(os.environ.get('HOOK_EXIT', '0')))\n"
        )
        original.chmod(0o755)
        nix = self.root / "nix"
        nix.write_text(
            f"#!{sys.executable}\n"
            "import json, os, pathlib, subprocess, sys\n"
            "args = sys.argv[1:]\n"
            "if args[:2] == ['eval', '--raw']:\n"
            "    if args[2] == '../#waddle-server-test-archive.outPath':\n"
            "        print(os.environ['EXPECTED_ARCHIVE'])\n"
            "    else:\n"
            "        assert args[2:4] == ['../#waddle-server-test-archive', '--apply']\n"
            "        print(os.environ['ARCHIVE_OUTPUTS'])\n"
            "elif args == ['config', 'show', 'post-build-hook']:\n"
            "    print(os.environ['ORIGINAL_HOOK'])\n"
            "elif args[0] == 'build':\n"
            "    pathlib.Path(os.environ['BUILD_LOG']).write_text(json.dumps(args))\n"
            "    if '--option' in args:\n"
            "        index = args.index('--option')\n"
            "        assert args[index + 1] == 'post-build-hook'\n"
            "        for paths in json.loads(os.environ['BUILT_OUTPUTS']):\n"
            "            result = subprocess.run([args[index + 2]], "
            "env=dict(os.environ, OUT_PATHS=' '.join(paths), DRV_PATH='/nix/store/test.drv'))\n"
            "            if result.returncode: sys.exit(result.returncode)\n"
            "    assert args[-1] == '../#waddle-server-test-archive^*'\n"
            "    if '--print-out-paths' in args: print(os.environ['ARCHIVE_OUTPUTS'])\n"
            "else:\n"
            "    sys.exit(99)\n"
        )
        nix.chmod(0o755)
        self.env = dict(
            os.environ, PATH=f"{self.root}:{os.environ['PATH']}",
            EXPECTED_ARCHIVE=ARCHIVE, ORIGINAL_HOOK=str(original),
            ARCHIVE_OUTPUTS="\n".join([ARCHIVE, *SHARDS]),
            BUILD_LOG=str(self.build_log), BUILT_OUTPUTS="[]",
        )

    def run_build(self, outputs):
        self.env["BUILT_OUTPUTS"] = json.dumps(outputs)
        return subprocess.run(["bash", str(SCRIPT)], env=self.env, capture_output=True, text=True)

    def hook_calls(self):
        return [json.loads(line) for line in self.hook_log.read_text().splitlines()]

    def assert_wrapper_removed(self):
        args = json.loads(self.build_log.read_text())
        self.assertFalse(Path(args[args.index("--option") + 2]).exists())

    def test_only_exact_archive_is_excluded_and_dependency_context_is_preserved(self):
        similar = ARCHIVE + "-related"
        result = self.run_build([[DEPENDENCY], [ARCHIVE, *SHARDS], [SHARDS[0], similar, DEPENDENCY]])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, ARCHIVE + "\n")
        self.assertEqual(self.hook_calls(), [
            {"paths": DEPENDENCY, "drv": "/nix/store/test.drv", "args": []},
            {"paths": f"{similar} {DEPENDENCY}", "drv": "/nix/store/test.drv", "args": []},
        ])
        self.assert_wrapper_removed()

    def test_no_configured_hook_keeps_ordinary_build(self):
        self.env["ORIGINAL_HOOK"] = ""
        result = self.run_build([[ARCHIVE]])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("--option", json.loads(self.build_log.read_text()))
        self.assertFalse(self.hook_log.exists())

    def test_original_hook_failure_propagates_and_wrapper_is_removed(self):
        self.env["HOOK_EXIT"] = "17"
        result = self.run_build([[DEPENDENCY]])
        self.assertEqual(result.returncode, 17)
        self.assert_wrapper_removed()

    def test_invalid_archive_path_cannot_disable_uploads(self):
        self.env["EXPECTED_ARCHIVE"] = "not-a-store-path"
        self.assertNotEqual(self.run_build([[DEPENDENCY]]).returncode, 0)
        self.assertFalse(self.build_log.exists())


if __name__ == "__main__":
    unittest.main()
