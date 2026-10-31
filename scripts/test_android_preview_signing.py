"""Exercise private signing material handling without repository credentials."""
import base64
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("android-preview-signing.sh")


class PreviewSigningTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fixture = tempfile.TemporaryDirectory()
        keystore = Path(cls.fixture.name) / "test.p12"
        subprocess.run([
            "keytool", "-genkeypair", "-keystore", str(keystore),
            "-storetype", "PKCS12", "-storepass", "test-password",
            "-alias", "waddle-preview", "-keyalg", "RSA", "-keysize", "2048",
            "-validity", "1", "-dname", "CN=Signing test", "-noprompt",
        ], check=True, capture_output=True)
        cls.encoded = base64.b64encode(keystore.read_bytes()).decode()

    @classmethod
    def tearDownClass(cls):
        cls.fixture.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.github_env = self.root / "github-env"
        self.env = {
            **os.environ,
            "RUNNER_TEMP": str(self.root),
            "GITHUB_ENV": str(self.github_env),
            "WADDLE_ANDROID_PREVIEW_KEYSTORE_BASE64": self.encoded,
            "WADDLE_ANDROID_PREVIEW_STORE_PASSWORD": "test-password",
        }
        self.env.pop("WADDLE_ANDROID_PREVIEW_SIGNING_DIR", None)

    def run_script(self, mode):
        return subprocess.run(["bash", str(SCRIPT), mode], env=self.env,
                              capture_output=True, text=True)

    def test_prepare_and_cleanup_keep_material_private(self):
        result = self.run_script("prepare")
        self.assertEqual(result.returncode, 0, result.stderr)
        signing_dir = Path(self.github_env.read_text().strip().split("=", 1)[1])
        self.assertEqual(signing_dir.stat().st_mode & 0o777, 0o700)
        for name in ("preview.p12", "store-password"):
            self.assertEqual((signing_dir / name).stat().st_mode & 0o777, 0o600)
        self.assertNotIn("test-password", self.github_env.read_text())
        self.env["WADDLE_ANDROID_PREVIEW_SIGNING_DIR"] = str(signing_dir)
        self.assertEqual(self.run_script("cleanup").returncode, 0)
        self.assertFalse(signing_dir.exists())

    def test_bad_keystore_is_removed_and_not_exported(self):
        self.env["WADDLE_ANDROID_PREVIEW_KEYSTORE_BASE64"] = "not-base64!"
        self.assertNotEqual(self.run_script("prepare").returncode, 0)
        self.assertFalse(list(self.root.glob("waddle-preview-signing.*")))
        self.assertFalse(self.github_env.exists())

    def test_missing_secret_fails_without_materializing(self):
        self.env.pop("WADDLE_ANDROID_PREVIEW_STORE_PASSWORD")
        self.assertNotEqual(self.run_script("prepare").returncode, 0)
        self.assertFalse(list(self.root.glob("waddle-preview-signing.*")))

    def test_cleanup_refuses_unrelated_directory(self):
        unrelated = self.root / "keep"
        unrelated.mkdir()
        self.env["WADDLE_ANDROID_PREVIEW_SIGNING_DIR"] = str(unrelated)
        self.assertNotEqual(self.run_script("cleanup").returncode, 0)
        self.assertTrue(unrelated.exists())


if __name__ == "__main__":
    unittest.main()
