#!/usr/bin/env bash
# Standalone Flutter outputs only. Never regenerates the Kotlin app's bindings.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MODE="${1:---android}"
if [[ $# -gt 1 ]]; then
  echo "Usage: $0 [--android|--host|--web]" >&2
  exit 2
fi

case "$MODE" in
  --android)
    ANDROID_HOME="${ANDROID_HOME:-$HOME/.android-sdk}"
    export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/28.2.13676358"
    test -d "$ANDROID_NDK_HOME" || {
      echo "Run scripts/setup-flutter-android.sh first (NDK 28.2.13676358 missing)." >&2
      exit 1
    }
    FLUTTER_JNI_STAGE="$(mktemp -d)"
    trap 'rm -rf "$FLUTTER_JNI_STAGE"' EXIT
    (cd "$REPO_ROOT/server" && cargo ndk \
      -t arm64-v8a -t x86_64 --platform 26 \
      -o "$FLUTTER_JNI_STAGE" \
      build --locked -p waddle-flutter-runtime --profile mobile)
    # cargo-ndk also copies dependency cdylibs. The runtime statically includes
    # its Rust dependencies; only its own C ABI belongs in this application's APK.
    for FLUTTER_ABI in arm64-v8a x86_64; do
      test -f "$FLUTTER_JNI_STAGE/$FLUTTER_ABI/libwaddle_flutter_runtime.so"
    done
    for FLUTTER_ABI in arm64-v8a x86_64; do
      FLUTTER_JNI_OUTPUT="$REPO_ROOT/apps/flutter/android/app/src/main/jniLibs/$FLUTTER_ABI"
      mkdir -p "$FLUTTER_JNI_OUTPUT"
      rm -f "$FLUTTER_JNI_OUTPUT/libwaddle_xmpp_client_ffi.so"
      cp "$FLUTTER_JNI_STAGE/$FLUTTER_ABI/libwaddle_flutter_runtime.so" "$FLUTTER_JNI_OUTPUT/"
    done
    ;;
  --host)
    (cd "$REPO_ROOT/server" && cargo build --locked -p waddle-flutter-runtime)
    ;;
  --web)
    # Flutter serves static assets; chat's Vite-specific ?url glue is unsuitable.
    # wasm-pack regenerates the JS and WASM as one matched pair in this app only.
    (cd "$REPO_ROOT/server/crates/waddle-xmpp-client-wasm" && wasm-pack build \
      --target web --out-name waddle_xmpp_client_wasm \
      --out-dir "$REPO_ROOT/apps/flutter/web/rust" -- --locked)
    ;;
  *) echo "Usage: $0 [--android|--host|--web]" >&2; exit 2 ;;
esac
