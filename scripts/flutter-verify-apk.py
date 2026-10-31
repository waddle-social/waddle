#!/usr/bin/env python3
"""Verify the standalone deliverable before it is promoted to an upload path."""

import hashlib
import os
from pathlib import Path
import re
import subprocess
import sys
import zipfile


def main():
    if len(sys.argv) != 4:
        raise SystemExit("Usage: flutter-verify-apk.py APK VERSION_CODE VERSION_NAME")
    # An absolute existing path cannot become an option to either SDK tool.
    apk = Path(sys.argv[1]).resolve(strict=True)
    expected_version, expected_name = sys.argv[2:]
    root = Path(__file__).resolve().parents[1]
    sdk = Path(os.environ.get("ANDROID_HOME", str(Path.home() / ".android-sdk")))
    tools = (sdk / "build-tools" / "36.0.0").resolve(strict=True)
    aapt = (tools / "aapt").resolve(strict=True)
    apksigner = (tools / "apksigner").resolve(strict=True)
    badging = subprocess.run(
        [str(aapt), "dump", "badging", str(apk)],
        check=True, capture_output=True, text=True,
    ).stdout
    package = re.search(r"^package: name='([^']+)' versionCode='([^']+)' versionName='([^']+)'", badging, re.M)
    if package is None or package.groups() != ("social.waddle.flutter", expected_version, expected_name):
        raise SystemExit("Standalone APK package/version differs from the requested build")
    with zipfile.ZipFile(apk) as archive:
        entries = set(archive.namelist())
    required = {f"lib/{abi}/libwaddle_flutter_runtime.so" for abi in ("arm64-v8a", "x86_64")}
    if not required.issubset(entries):
        raise SystemExit("Standalone APK is missing a required direct Rust ABI")
    if any(name.endswith("/libwaddle_xmpp_client_ffi.so") for name in entries):
        raise SystemExit("Standalone APK contains the original Android JNI library")
    certificate = subprocess.run(
        ["keytool", "-exportcert", "-keystore", str(root / "apps/flutter/android/preview.keystore"),
         "-alias", "waddle-flutter-preview", "-storepass", "android"],
        check=True, capture_output=True,
    ).stdout
    expected_digest = hashlib.sha256(certificate).hexdigest()
    verified = subprocess.run(
        [str(apksigner), "verify", "--print-certs", str(apk)],
        check=True, capture_output=True, text=True,
    ).stdout
    signers = re.findall(r"^Signer #\d+ certificate SHA-256 digest: ([0-9a-fA-F]+)$", verified, re.M)
    if len(signers) != 1 or signers[0].lower() != expected_digest:
        raise SystemExit("Standalone APK signer differs from the committed Flutter preview key")
    print("Verified alternative Flutter package, version, two Rust ABIs and preview signer")


if __name__ == "__main__":
    main()
