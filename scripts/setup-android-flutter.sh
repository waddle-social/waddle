#!/usr/bin/env bash
# Generate the ignored add-to-app Android wrapper and resolve Dart packages.
# The SDK comes from the dev shell, PATH, or an explicitly selected FLUTTER_ROOT.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
flutter_command="${FLUTTER_ROOT:+${FLUTTER_ROOT}/bin/}flutter"
if ! command -v "$flutter_command" >/dev/null 2>&1; then
  printf '%s\n' 'Flutter SDK is required. Enter the repository nix dev shell or install Flutter and add it to PATH.' >&2
  exit 1
fi

flutter_version_json="$("$flutter_command" --suppress-analytics --version --machine)"
printf '%s' "$flutter_version_json" | python3 -c '
import json, re, sys
version = json.load(sys.stdin).get("frameworkVersion", "")
match = re.match(r"^(\d+)\.(\d+)\.(\d+)", version)
if not match or tuple(map(int, match.groups())) < (3, 44, 0):
    sys.exit("Flutter 3.44 or later is required for the Android AGP 9 host; found " + version)
'

cd "$REPO_ROOT/apps/android/flutter"
"$flutter_command" --suppress-analytics pub get
if [[ ! -f .android/include_flutter.groovy ]]; then
  printf '%s\n' 'Flutter did not generate its Android module wrapper. Check that pubspec.yaml declares flutter.module.' >&2
  exit 1
fi
