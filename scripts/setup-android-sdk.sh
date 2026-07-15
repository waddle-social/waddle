#!/usr/bin/env bash
# Install the pinned Android SDK packages the waddle Android app needs.
#
# Usage:
#   ./scripts/setup-android-sdk.sh
#
# Idempotent: a sentinel keyed on this script's content hash makes repeat
# runs a no-op. Honors an existing $ANDROID_HOME (e.g. ~/Library/Android/sdk
# from Android Studio) and only adds the missing packages; defaults to
# ~/.android-sdk on machines with no SDK.
#
# Writes apps/android/local.properties (sdk.dir) so Gradle resolves the SDK
# without needing ANDROID_HOME to survive cuenv's environment scrubbing.
#
# Requires: JAVA_HOME pointing at a JDK 17+ (sdkmanager is a JVM tool).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

CMDLINE_TOOLS_BUILD="14742923"
CMDLINE_TOOLS_SHA256_MAC="ed304c5ede3718541e4f978e4ae870a4d853db74af6c16d920588d48523b9dee"
CMDLINE_TOOLS_SHA256_LINUX="04453066b540409d975c676d781da1477479dde3761310f1a7eb92a1dfb15af7"

# Keep in lockstep with apps/android Gradle config and
# scripts/build-android-rust.sh.
NDK_VERSION="28.2.13676358"
PACKAGES=(
  "platform-tools"
  # Android 17 = API 37; Google ships it under the minor-versioned
  # package id introduced with the 36.1/37.0 SDK release scheme.
  "platforms;android-37.0"
  "platforms;android-36"
  "build-tools;36.0.0"
  "ndk;${NDK_VERSION}"
)

case "$(uname -s)" in
  Darwin) OS_TAG="mac"; EXPECTED_SHA="$CMDLINE_TOOLS_SHA256_MAC" ;;
  Linux) OS_TAG="linux"; EXPECTED_SHA="$CMDLINE_TOOLS_SHA256_LINUX" ;;
  *) echo "unsupported host for the Android SDK: $(uname -s)" >&2; exit 1 ;;
esac

ANDROID_HOME="${ANDROID_HOME:-$HOME/.android-sdk}"
SENTINEL="$ANDROID_HOME/.waddle-sdk-rev"

script_rev() {
  shasum -a 256 "${BASH_SOURCE[0]}" | cut -d' ' -f1
}

write_local_properties() {
  printf 'sdk.dir=%s\n' "$ANDROID_HOME" > "$REPO_ROOT/apps/android/local.properties"
}

if [[ -f "$SENTINEL" && "$(cat "$SENTINEL")" == "$(script_rev)" ]]; then
  write_local_properties
  echo "Android SDK already provisioned at $ANDROID_HOME"
  exit 0
fi

SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
if [[ ! -x "$SDKMANAGER" ]]; then
  echo "==> Installing Android command-line tools (build $CMDLINE_TOOLS_BUILD)"
  TMP_DIR="$(mktemp -d)"
  trap 'rm -rf "$TMP_DIR"' EXIT
  ZIP="$TMP_DIR/commandlinetools.zip"
  curl -fsSL -o "$ZIP" \
    "https://dl.google.com/android/repository/commandlinetools-${OS_TAG}-${CMDLINE_TOOLS_BUILD}_latest.zip"
  echo "$EXPECTED_SHA  $ZIP" | shasum -a 256 --check --quiet
  unzip -q "$ZIP" -d "$TMP_DIR"
  mkdir -p "$ANDROID_HOME/cmdline-tools"
  rm -rf "$ANDROID_HOME/cmdline-tools/latest"
  mv "$TMP_DIR/cmdline-tools" "$ANDROID_HOME/cmdline-tools/latest"
fi

echo "==> Accepting licenses"
# `< <(yes)` instead of `yes |`: under pipefail, `yes` dying of SIGPIPE
# when sdkmanager stops reading would abort the whole script.
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" --licenses < <(yes) > /dev/null

echo "==> Installing packages: ${PACKAGES[*]}"
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" "${PACKAGES[@]}"

write_local_properties
script_rev > "$SENTINEL"
echo "==> Done: $ANDROID_HOME"
