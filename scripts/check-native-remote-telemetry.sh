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
collector_pattern='COLLECTOR_(URL|ENDPOINT)|TELEMETRY_(URL|ENDPOINT|COLLECTOR)|OTEL_EXPORTER|OTEL_ENDPOINT|SENTRY_DSN|FARO_URL|https?://[^"[:space:]]*(telemetry|collector|/v1/traces|/v1/metrics|/collect)'

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
  if grep -ERni -- "$pattern" "${existing[@]}" >"$output_file" 2>/dev/null; then
    echo "[fail] $label matched pattern: $pattern" >&2
    cat "$output_file" >&2
    failed=1
  fi
  rm -f "$output_file"
}

check_apple() {
  require_file "apps/apple/project.yml"
  require_file "apps/apple/Waddle.xcodeproj/project.pbxproj"
  require_file "apps/apple/Waddle/App/AppModel.swift"
  require_file "apps/apple/Waddle/RustClient/RustXmppClient.swift"
  check_absent "apple dependency" \
    "sentry|crashlytics|firebase|datadog|newrelic|mixpanel|amplitude|posthog|bugsnag|telemetrydeck|faro|otlp|opentelemetry" \
    "$repo_root/apps/apple/project.yml" \
    "$repo_root/apps/apple/Waddle.xcodeproj/project.pbxproj" \
    "$repo_root/apps/apple/Waddle"
  check_absent "apple exporter init" \
    "SentrySDK\\.start|FirebaseApp\\.configure|Crashlytics|Datadog|NewRelic|OTLP|OpenTelemetry|BatchSpanProcessor|SimpleSpanProcessor|TracerProvider|${collector_pattern}" \
    "$repo_root/apps/apple/Waddle"
}

check_android() {
  require_file "apps/android/app/build.gradle.kts"
  require_file "apps/android/core/client/build.gradle.kts"
  check_absent "android dependency" \
    "sentry|crashlytics|firebase-analytics|datadog|newrelic|mixpanel|amplitude|posthog|bugsnag|faro|otlp|opentelemetry" \
    "$repo_root/apps/android/build.gradle.kts" \
    "$repo_root/apps/android/app/build.gradle.kts" \
    "$repo_root/apps/android/core/client/build.gradle.kts" \
    "$repo_root/apps/android/gradle.properties" \
    "$repo_root/apps/android/app/src/main/AndroidManifest.xml"
  check_absent "android exporter init" \
    "Sentry\\.init|FirebaseApp\\.initializeApp|FirebaseCrashlytics|FirebaseAnalytics|Datadog|NewRelic|OpenTelemetry|Otlp|BatchSpanProcessor|SimpleSpanProcessor|${collector_pattern}" \
    "$repo_root/apps/android/build.gradle.kts" \
    "$repo_root/apps/android/app/build.gradle.kts" \
    "$repo_root/apps/android/core/client/build.gradle.kts" \
    "$repo_root/apps/android/gradle.properties" \
    "$repo_root/apps/android/app/src/main/AndroidManifest.xml" \
    "$repo_root/apps/android/app/src/main" \
    "$repo_root/apps/android/core/client/src/main"
}

check_wasm() {
  require_file "server/crates/waddle-xmpp-client-wasm/Cargo.toml"
  require_file "server/crates/waddle-xmpp-client-wasm/src/events.rs"
  require_file "server/crates/waddle-xmpp-client-wasm/src/driver.rs"
  require_file "server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.js"
  check_absent "wasm dependency" \
    "sentry|datadog|newrelic|mixpanel|amplitude|posthog|bugsnag|faro|otlp|opentelemetry|tracing-opentelemetry|tracing-subscriber" \
    "$repo_root/server/crates/waddle-xmpp-client-wasm/Cargo.toml"
  check_absent "wasm exporter init" \
    "tracing_subscriber|set_global_default|install_batch|install_simple|sentry::init|opentelemetry_otlp|tracing_opentelemetry|${collector_pattern}" \
    "$repo_root/server/crates/waddle-xmpp-client-wasm/src" \
    "$repo_root/server/wasm-pkg/waddle-xmpp-client-wasm"
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
