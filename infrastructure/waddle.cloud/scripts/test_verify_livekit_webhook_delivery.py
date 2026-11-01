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
SECRET_MARKER = "synthetic-secret-that-must-never-appear-in-output"


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
                [sys.executable, str(VERIFIER), "--context", "synthetic-context", "--temporary-egress"],
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


if __name__ == "__main__":
    unittest.main()

