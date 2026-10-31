#!/usr/bin/env bash
# PR APKs share the rolling build's signature and monotonic version convention.
# Compute from the commit rather than shallow clone depth or CI run numbers.
set -euo pipefail

android_version_code="$(( $(git show -s --format=%ct HEAD) / 60 ))"
android_version_name="pr-$(git rev-parse --short HEAD)"
exec bash "$(dirname "${BASH_SOURCE[0]}")/android-ci-gradle.sh" \
  "$@" \
  "-PversionCode=${android_version_code}" \
  "-PversionName=${android_version_name}"
