#!/usr/bin/env bash
# Independent SDK provisioning. The native Android app and its local.properties
# are deliberately untouched. SDK/NDK pins match the reviewed native SDK setup.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_HOME="${ANDROID_HOME:-$HOME/.android-sdk}"
export ANDROID_HOME
NDK_VERSION="28.2.13676358"
CMDLINE_TOOLS_BUILD="14742923"
case "$(uname -s)" in
  Linux) OS_TAG=linux; EXPECTED_SHA=04453066b540409d975c676d781da1477479dde3761310f1a7eb92a1dfb15af7 ;;
  Darwin) OS_TAG=mac; EXPECTED_SHA=ed304c5ede3718541e4f978e4ae870a4d853db74af6c16d920588d48523b9dee ;;
  *) echo "Unsupported Android SDK host" >&2; exit 1 ;;
esac

SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
if [[ ! -x "$SDKMANAGER" ]]; then
  SDK_DOWNLOAD="$(mktemp -d)"
  trap 'rm -rf "$SDK_DOWNLOAD"' EXIT
  curl --proto '=https' --proto-redir '=https' -fsSL \
    -o "$SDK_DOWNLOAD/tools.zip" \
    "https://dl.google.com/android/repository/commandlinetools-${OS_TAG}-${CMDLINE_TOOLS_BUILD}_latest.zip"
  printf '%s  %s\n' "$EXPECTED_SHA" "$SDK_DOWNLOAD/tools.zip" | shasum -a 256 --check --quiet
  unzip -q "$SDK_DOWNLOAD/tools.zip" -d "$SDK_DOWNLOAD"
  mkdir -p "$ANDROID_HOME/cmdline-tools/latest"
  cp -R "$SDK_DOWNLOAD/cmdline-tools/." "$ANDROID_HOME/cmdline-tools/latest/"
fi

# Avoid pipefail interpreting yes's expected SIGPIPE as a license failure.
set +o pipefail
yes | "$SDKMANAGER" --sdk_root="$ANDROID_HOME" --licenses >/dev/null
LICENSE_STATUS=${PIPESTATUS[1]}
set -o pipefail
test "$LICENSE_STATUS" -eq 0
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" \
  "platform-tools" "platforms;android-37.0" "platforms;android-36" \
  "build-tools;36.0.0" "ndk;$NDK_VERSION"

test -d "$REPO_ROOT/apps/flutter/android"
printf 'sdk.dir=%s\n' "$ANDROID_HOME" > "$REPO_ROOT/apps/flutter/android/local.properties"
