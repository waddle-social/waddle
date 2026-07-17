#!/usr/bin/env bash
# Build libwaddle_xmpp_client_ffi.so for Android and regenerate the
# committed Kotlin bindings.
#
# Usage:
#   ./scripts/build-android-rust.sh [--debug] [--skip-bindings]
#
# Outputs:
#   apps/android/core/client/src/main/jniLibs/{arm64-v8a,x86_64}/libwaddle_xmpp_client_ffi.so
#   apps/android/core/client/src/main/kotlin/social/waddle/client/ffi/waddle_xmpp_client.kt (committed)
#
# Prerequisites:
#   scripts/setup-android-sdk.sh (provides the pinned NDK)
#   cargo-ndk (nix devshell, or: cargo install --locked cargo-ndk)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SERVER="$REPO_ROOT/server"
CARGO_TARGET_ROOT="${CARGO_TARGET_DIR:-$SERVER/target}"
if [[ "$CARGO_TARGET_ROOT" != /* ]]; then
  CARGO_TARGET_ROOT="$REPO_ROOT/$CARGO_TARGET_ROOT"
fi
export CARGO_TARGET_DIR="$CARGO_TARGET_ROOT"
TMP_ROOT="${TMPDIR:-/tmp}"
if [[ "$TMP_ROOT" != /* ]]; then
  TMP_ROOT="$REPO_ROOT/$TMP_ROOT"
fi
mkdir -p "$TMP_ROOT"
export TMPDIR="$TMP_ROOT"
JNILIBS="$REPO_ROOT/apps/android/core/client/src/main/jniLibs"
BINDINGS_DIR="$REPO_ROOT/apps/android/core/client/src/main/kotlin"
BINDINGS_FILE="$BINDINGS_DIR/social/waddle/client/ffi/waddle_xmpp_client.kt"

# Keep in lockstep with scripts/setup-android-sdk.sh and the
# android.ndkVersion pin in apps/android Gradle config.
NDK_VERSION="28.2.13676358"
# = minSdk of the Android app.
ANDROID_API="34"

PROFILE="release"
CARGO_FLAG="--release"
SKIP_BINDINGS=0
for arg in "$@"; do
  case "$arg" in
    --debug) PROFILE="debug"; CARGO_FLAG="" ;;
    --skip-bindings) SKIP_BINDINGS=1 ;;
    *) echo "unknown argument: $arg" >&2; exit 1 ;;
  esac
done

# SDK discovery: env > local.properties default location. cuenv scrubs the
# ambient environment, so the setup script's default matters in CI.
ANDROID_HOME="${ANDROID_HOME:-$HOME/.android-sdk}"
if [[ ! -d "$ANDROID_HOME/ndk/$NDK_VERSION" && -d "$HOME/Library/Android/sdk/ndk/$NDK_VERSION" ]]; then
  ANDROID_HOME="$HOME/Library/Android/sdk"
fi
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/$NDK_VERSION"
if [[ ! -d "$ANDROID_NDK_HOME" ]]; then
  echo "NDK $NDK_VERSION not found; run scripts/setup-android-sdk.sh first" >&2
  exit 1
fi

echo "==> Building Android targets (profile: $PROFILE)"
(cd "$SERVER" && cargo ndk \
  -t arm64-v8a \
  -t x86_64 \
  --platform "$ANDROID_API" \
  -o "$JNILIBS" \
  build -p waddle-xmpp-client-ffi --locked $CARGO_FLAG)

if [[ "$SKIP_BINDINGS" == "1" ]]; then
  exit 0
fi

case "$(uname -s)" in
  Darwin) LIB_EXT="dylib" ;;
  Linux) LIB_EXT="so" ;;
  *) echo "unsupported host for UniFFI binding generation: $(uname -s)" >&2; exit 1 ;;
esac

echo "==> Generating Kotlin bindings"
(cd "$SERVER" && cargo build -p waddle-xmpp-client-ffi --locked)
(cd "$SERVER" && cargo run -p waddle-xmpp-client-ffi \
  --locked \
  --bin uniffi-bindgen \
  --features waddle-xmpp-client-ffi/uniffi-bindgen-bin \
  -- generate \
  --library "$CARGO_TARGET_ROOT/debug/libwaddle_xmpp_client_ffi.${LIB_EXT}" \
  --language kotlin \
  --no-format \
  --out-dir "$BINDINGS_DIR")

perl -pi -e 's/[ \t]+$//' "$BINDINGS_FILE"

echo "==> Done: $JNILIBS ($PROFILE) + $BINDINGS_FILE"
