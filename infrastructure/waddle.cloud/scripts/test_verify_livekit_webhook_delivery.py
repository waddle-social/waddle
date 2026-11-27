"""CLI credential failures must never echo Kubernetes secret contents."""

import base64
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


VERIFIER = Path(__file__).with_name("verify-livekit-webhook-delivery.py")
SECRET_MARKER = "sensitive-fixture-secret"


class WebhookCredentialRedactionTests(unittest.TestCase):
    def run_invalid_credentials(self, keys_yaml):
        def encode(value):
            return base64.b64encode(value.encode()).decode()

        with tempfile.TemporaryDirectory() as directory:
            fake_kubectl = Path(directory, "kubectl")
            fake_kubectl.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "args = sys.argv[1:]\n"
                "if args == ['config', 'get-contexts', '-o', 'name']:\n"
                "    print('synthetic-context'); raise SystemExit(0)\n"
                "if args[0] != '--context=synthetic-context':\n"
                "    raise SystemExit('context must be passed as one validated argument')\n"
                "if 'exec' in args or 'create' in args or 'delete' in args:\n"
                "    raise SystemExit('credential failure must precede any probe or mutation')\n"
                "name = args[args.index('secret') + 3]\n"
                "print(os.environ['SFU_FIXTURE'] if name == 'livekit-sfu-api-keys' else os.environ['SERVER_FIXTURE'])\n"
            )
            fake_kubectl.chmod(0o755)
            environment = {
                **os.environ,
                "PATH": directory + os.pathsep + os.environ["PATH"],
                "SFU_FIXTURE": json.dumps({"data": {
                    "webhook-key": encode("fixture-signer"),
                    "keys.yaml": encode(keys_yaml),
                }}),
                "SERVER_FIXTURE": json.dumps({"data": {
                    "LIVEKIT_WEBHOOK_SECRET": encode(SECRET_MARKER),
                }}),
            }
            return subprocess.run(
                [sys.executable, str(VERIFIER), "--context", "synthetic-context"],
                capture_output=True, text=True, env=environment, check=False,
            )

    def test_malformed_yaml_is_redacted_at_the_cli_boundary(self):
        result = self.run_invalid_credentials('fixture-signer: "' + SECRET_MARKER + '\n')
        output = result.stdout + result.stderr
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn(SECRET_MARKER, output)
        self.assertNotIn("Traceback", output)
        self.assertIn("webhook credentials are malformed or incomplete", output)

    def test_non_string_signer_secrets_fail_without_input_or_traceback(self):
        for keys_yaml in [
            "fixture-signer: [" + SECRET_MARKER + "]",
            "- " + SECRET_MARKER,
            SECRET_MARKER,
            "different-signer: " + SECRET_MARKER,
        ]:
            with self.subTest(keys_yaml=keys_yaml):
                result = self.run_invalid_credentials(keys_yaml)
                output = result.stdout + result.stderr
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn(SECRET_MARKER, output)
                self.assertNotIn("Traceback", output)
                self.assertIn("webhook credentials are malformed or incomplete", output)


class WebhookContextValidationTests(unittest.TestCase):
    def test_invalid_or_unknown_contexts_never_read_secrets_or_probe(self):
        with tempfile.TemporaryDirectory() as directory:
            calls = Path(directory, "calls.jsonl")
            fake_kubectl = Path(directory, "kubectl")
            fake_kubectl.write_text(
                f"#!{sys.executable}\n"
                "import json, os, sys\n"
                "with open(os.environ['KUBECTL_CALLS'], 'a') as calls:\n"
                "    calls.write(json.dumps(sys.argv[1:]) + '\\n')\n"
                "if sys.argv[1:] == ['config', 'get-contexts', '-o', 'name']:\n"
                "    print('synthetic-context')\n"
                "else:\n"
                "    raise SystemExit('unexpected cluster access')\n"
            )
            fake_kubectl.chmod(0o755)
            environment = {
                **os.environ,
                "PATH": directory + os.pathsep + os.environ["PATH"],
                "KUBECTL_CALLS": str(calls),
            }
            for context in ["--namespace=other", "synthetic-context\n", "synthetic-context\t", "unknown-context"]:
                with self.subTest(context=context):
                    calls.write_text("")
                    result = subprocess.run(
                        [sys.executable, str(VERIFIER), "--context=" + context],
                        capture_output=True, text=True, env=environment, check=False,
                    )
                    output = result.stdout + result.stderr
                    self.assertNotEqual(result.returncode, 0)
                    self.assertNotIn("Traceback", output)
                    self.assertIn("Kubernetes context", output)
                    requests = [json.loads(line) for line in calls.read_text().splitlines()]
                    self.assertTrue(all(request == ["config", "get-contexts", "-o", "name"] for request in requests))
                    if context == "unknown-context":
                        self.assertEqual(requests, [["config", "get-contexts", "-o", "name"]])


if __name__ == "__main__":
    unittest.main()
