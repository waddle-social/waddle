#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="${WADDLE_NATIVE_TELEMETRY_ROOT:-$(cd "${script_dir}/.." && pwd)}"

usage() {
  echo "usage: scripts/check-native-remote-telemetry.sh --apple|--android|--wasm|--all" >&2
}

if [[ $# -ne 1 ]]; then
  usage
  exit 2
fi

mode="$1"
case "${mode}" in
  --apple|--android|--wasm|--all) ;;
  *)
    usage
    exit 2
    ;;
esac

failed=0
collector_pattern='COLLECTOR_(URL|ENDPOINT)|TELEMETRY_(URL|ENDPOINT|COLLECTOR)|OTEL_EXPORTER|OTEL_ENDPOINT|SENTRY_DSN|FARO_URL|https?://[^"[:space:]]{0,160}(telemetry|collector|/v1/traces|/v1/metrics|/collect)'
native_dependency_pattern='sentry|crashlytics|firebase|firebase-analytics|datadog|newrelic|mixpanel|amplitude|posthog|bugsnag|telemetrydeck|faro|otlp|opentelemetry|tracing-opentelemetry|tracing-subscriber'
native_exporter_pattern="tracing_subscriber|set_global_default|install_batch|install_simple|sentry::init|Sentry\\.init|SentrySDK\\.start|FirebaseApp\\.configure|FirebaseApp\\.initializeApp|Crashlytics|FirebaseCrashlytics|FirebaseAnalytics|Datadog|NewRelic|OpenTelemetry|Otlp|OTLP|BatchSpanProcessor|SimpleSpanProcessor|TracerProvider|opentelemetry_otlp|tracing_opentelemetry|${collector_pattern}"

require_file() {
  local rel="$1"
  if [[ ! -f "${repo_root}/${rel}" ]]; then
    echo "[fail] missing required file: ${rel}" >&2
    failed=1
  fi
}

check_absent() {
  local label="$1"
  local pattern="$2"
  shift 2
  local -a existing=()
  local path
  for path in "$@"; do
    if [[ -e "$path" ]]; then
      existing+=("$path")
    fi
  done
  if [[ ${#existing[@]} -eq 0 ]]; then
    return
  fi
  local output_file
  output_file="$(mktemp)"
  if command -v rg >/dev/null 2>&1; then
    if rg --hidden --glob '!.git' --glob '!target' --no-messages -n -i -e "$pattern" "${existing[@]}" >"$output_file"; then
      echo "[fail] $label matched pattern: $pattern" >&2
      cat "$output_file" >&2
      failed=1
    fi
  else
    if grep -ERniI -- "$pattern" "${existing[@]}" >"$output_file" 2>/dev/null; then
      echo "[fail] $label matched pattern: $pattern" >&2
      cat "$output_file" >&2
      failed=1
    fi
  fi
  rm -f "$output_file"
}

check_apple() {
  require_file "apps/apple/project.yml"
  require_file "apps/apple/Waddle.xcodeproj/project.pbxproj"
  require_file "apps/apple/Waddle/App/AppModel.swift"
  require_file "apps/apple/Waddle/RustClient/RustXmppClient.swift"
  require_file "server/crates/waddle-xmpp-client/Cargo.toml"
  require_file "server/crates/waddle-xmpp-core/Cargo.toml"
  require_file "server/crates/waddle-xmpp-client-ffi/Cargo.toml"
  local -a apple_dependency_paths=(
    "$repo_root/apps/apple/project.yml"
    "$repo_root/apps/apple/Waddle.xcodeproj/project.pbxproj"
    "$repo_root/server/crates/waddle-xmpp-client/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-client/src"
    "$repo_root/server/crates/waddle-xmpp-core/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-core/src"
    "$repo_root/server/crates/waddle-xmpp-client-ffi/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-client-ffi/src"
  )
  local -a apple_exporter_paths=(
    "$repo_root/apps/apple/Waddle"
    "$repo_root/server/crates/waddle-xmpp-client/src"
    "$repo_root/server/crates/waddle-xmpp-core/src"
    "$repo_root/server/crates/waddle-xmpp-client-ffi/src"
  )
  check_absent "apple dependency" \
    "$native_dependency_pattern" \
    "${apple_dependency_paths[@]}"
  check_absent "apple exporter init" \
    "$native_exporter_pattern" \
    "${apple_exporter_paths[@]}"
}

check_android() {
  local -a android_dependency_paths=(
    "$repo_root/apps/android/build.gradle.kts"
    "$repo_root/apps/android/settings.gradle.kts"
    "$repo_root/apps/android/gradle/libs.versions.toml"
    "$repo_root/apps/android/gradle.properties"
    "$repo_root/apps/android/app/src/main/AndroidManifest.xml"
    "$repo_root/server/crates/waddle-xmpp-client/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-client/src"
    "$repo_root/server/crates/waddle-xmpp-core/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-core/src"
    "$repo_root/server/crates/waddle-xmpp-client-ffi/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-client-ffi/src"
  )
  local -a android_exporter_paths=(
    "$repo_root/apps/android/build.gradle.kts"
    "$repo_root/apps/android/settings.gradle.kts"
    "$repo_root/apps/android/gradle.properties"
    "$repo_root/apps/android/app/src/main/AndroidManifest.xml"
    "$repo_root/apps/android/app/src/main"
    "$repo_root/apps/android/core/client/src/main"
    "$repo_root/server/crates/waddle-xmpp-client/src"
    "$repo_root/server/crates/waddle-xmpp-core/src"
    "$repo_root/server/crates/waddle-xmpp-client-ffi/src"
  )
  local path
  while IFS= read -r path; do
    android_dependency_paths+=("$path")
    android_exporter_paths+=("$path")
  done < <(find "$repo_root/apps/android" -type f -name "build.gradle.kts" 2>/dev/null)
  require_file "apps/android/build.gradle.kts"
  require_file "apps/android/app/build.gradle.kts"
  require_file "apps/android/core/client/build.gradle.kts"
  require_file "apps/android/settings.gradle.kts"
  require_file "apps/android/gradle/libs.versions.toml"
  require_file "server/crates/waddle-xmpp-client/Cargo.toml"
  require_file "server/crates/waddle-xmpp-core/Cargo.toml"
  require_file "server/crates/waddle-xmpp-client-ffi/Cargo.toml"
  check_absent "android dependency" \
    "$native_dependency_pattern" \
    "${android_dependency_paths[@]}"
  check_absent "android exporter init" \
    "$native_exporter_pattern" \
    "${android_exporter_paths[@]}"
}

check_wasm() {
  local -a wasm_dependency_paths=(
    "$repo_root/server/crates/waddle-xmpp-client-wasm/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-client-wasm/src"
    "$repo_root/server/crates/waddle-xmpp-client/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-client/src"
    "$repo_root/server/crates/waddle-xmpp-core/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-core/src"
    "$repo_root/server/crates/waddle-xmpp-client-ffi/Cargo.toml"
    "$repo_root/server/crates/waddle-xmpp-client-ffi/src"
  )
  local -a wasm_exporter_paths=(
    "$repo_root/server/crates/waddle-xmpp-client-wasm/src"
    "$repo_root/server/wasm-pkg/waddle-xmpp-client-wasm"
    "$repo_root/server/crates/waddle-xmpp-client/src"
    "$repo_root/server/crates/waddle-xmpp-core/src"
    "$repo_root/server/crates/waddle-xmpp-client-ffi/src"
  )
  require_file "server/crates/waddle-xmpp-client-wasm/Cargo.toml"
  require_file "server/crates/waddle-xmpp-client-wasm/src/events.rs"
  require_file "server/crates/waddle-xmpp-client-wasm/src/driver.rs"
  require_file "server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.js"
  require_file "server/crates/waddle-xmpp-client/Cargo.toml"
  require_file "server/crates/waddle-xmpp-core/Cargo.toml"
  require_file "server/crates/waddle-xmpp-client-ffi/Cargo.toml"
  check_absent "wasm dependency" \
    "$native_dependency_pattern" \
    "${wasm_dependency_paths[@]}"
  check_absent "wasm exporter init" \
    "$native_exporter_pattern" \
    "${wasm_exporter_paths[@]}"
}

case "${mode}" in
  --apple)
    check_apple
    ;;
  --android)
    check_android
    ;;
  --wasm)
    check_wasm
    ;;
  --all)
    check_apple
    check_android
    check_wasm
    ;;
  *)
    usage
    ;;
esac

if [[ $failed -ne 0 ]]; then
  exit 1
fi

echo "native remote telemetry contract OK: ${mode#--}"
