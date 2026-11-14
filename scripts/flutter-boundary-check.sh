#!/usr/bin/env bash
# A PR for the alternative client must not alter the existing Android app.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"
FLUTTER_NATIVE_BASE="${WADDLE_NATIVE_BASE_SHA:-12fd7aa01dbef427c383fd16fed00b83badbb84f}"
git cat-file -e "${FLUTTER_NATIVE_BASE}^{commit}"
git diff --exit-code "$FLUTTER_NATIVE_BASE" -- \
  apps/android \
  .github/workflows/waddle-android-default.yml \
  .github/workflows/waddle-android-devicetests.yml \
  .github/workflows/waddle-android-pullrequest.yml \
  scripts/android-ci-gradle.sh scripts/build-android-rust.sh \
  scripts/check-android-ffi-bindings.sh scripts/setup-android-sdk.sh \
  scripts/test_android_ci_gradle.py
if rg -n 'social\.waddle\.android/|social\.waddle\.android/call-video|\bAppGraph\b' apps/flutter/lib; then
  echo "Standalone Flutter must not call the native Android application's facade." >&2
  exit 1
fi
