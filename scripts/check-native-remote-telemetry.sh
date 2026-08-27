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
native_dependency_pattern='(^|[^[:alnum:]_])(sentry|crashlytics|firebase([_-]analytics)?|datadog|newrelic|mixpanel|amplitude|posthog|bugsnag|telemetrydeck|faro|otlp|opentelemetry([_-]otlp)?|tracing[_-](opentelemetry|subscriber))([^[:alnum:]_]|$)'
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

resolve_dir() {
  local dir="$1"
  (
    cd "$dir" >/dev/null 2>&1 && pwd
  )
}

cargo_path_dependencies() {
  local manifest="$1"
  awk '
    /^[[:space:]]*\[/ {
      section = $0
      gsub(/^[[:space:]]*\[/, "", section)
      sub(/\].*$/, "", section)
      active = section == "dependencies" || section == "build-dependencies" || section ~ /^target\..*\.(dependencies|build-dependencies)$/ || section ~ /^(dependencies|build-dependencies)\.[^.]+$/ || section ~ /^target\..*\.(dependencies|build-dependencies)\.[^.]+$/
    }
    active && $0 !~ /^[[:space:]]*#/ {
      if (match($0, /path[[:space:]]*=[[:space:]]*"[^"]+"/)) {
        value = substr($0, RSTART, RLENGTH)
        sub(/^[^"]*"/, "", value)
        sub(/"$/, "", value)
        print value
      } else if (match($0, /path[[:space:]]*=[[:space:]]*\047[^\047]+\047/)) {
        value = substr($0, RSTART, RLENGTH)
        sub(/^[^\047]*\047/, "", value)
        sub(/\047$/, "", value)
        print value
      }
    }
  ' "$manifest"
}

# Dependency names a manifest inherits from its workspace root
# (`name = { workspace = true }` / `name.workspace = true`) inside
# dependency sections.
cargo_workspace_inherited_dependencies() {
  local manifest="$1"
  awk '
    /^[[:space:]]*\[/ {
      section = $0
      gsub(/^[[:space:]]*\[/, "", section)
      sub(/\].*$/, "", section)
      active = section == "dependencies" || section == "build-dependencies" || section ~ /^target\..*\.(dependencies|build-dependencies)$/
      inline_name = ""
      if (section ~ /^(dependencies|build-dependencies)\.[^.]+$/ || section ~ /^target\..*\.(dependencies|build-dependencies)\.[^.]+$/) {
        inline_name = section
        sub(/^.*\./, "", inline_name)
      }
    }
    $0 ~ /^[[:space:]]*#/ { next }
    active && $0 ~ /^[[:space:]]*[A-Za-z0-9_-]+([[:space:]]*=[[:space:]]*\{[^}]*workspace[[:space:]]*=[[:space:]]*true|\.workspace[[:space:]]*=[[:space:]]*true)/ {
      name = $0
      sub(/^[[:space:]]*/, "", name)
      sub(/[[:space:]]*(=|\.workspace).*$/, "", name)
      print name
    }
    inline_name != "" && $0 ~ /^[[:space:]]*workspace[[:space:]]*=[[:space:]]*true/ {
      print inline_name
    }
  ' "$manifest"
}

# Nearest ancestor Cargo.toml declaring `[workspace]`, or nothing.
cargo_workspace_root_manifest() {
  local dir="$1"
  while [[ -n "$dir" && "$dir" != "/" ]]; do
    if [[ -f "${dir}/Cargo.toml" ]] && grep -Eq '^[[:space:]]*\[workspace\]' "${dir}/Cargo.toml"; then
      printf '%s\n' "${dir}/Cargo.toml"
      return 0
    fi
    dir="$(dirname "$dir")"
  done
  return 0
}

# `path = "..."` of one `[workspace.dependencies]` entry, relative to the
# workspace root.
cargo_workspace_dependency_path() {
  local root_manifest="$1"
  local name="$2"
  awk -v name="$name" '
    /^[[:space:]]*\[/ {
      section = $0
      gsub(/^[[:space:]]*\[/, "", section)
      sub(/\].*$/, "", section)
      active = section == "workspace.dependencies"
      inline = section == "workspace.dependencies." name
    }
    $0 ~ /^[[:space:]]*#/ { next }
    (active && index($0, name) == match($0, /[A-Za-z0-9_-]+/) && substr($0, RSTART, RLENGTH) == name) || inline {
      if (match($0, /path[[:space:]]*=[[:space:]]*"[^"]+"/)) {
        value = substr($0, RSTART, RLENGTH)
        sub(/^[^"]*"/, "", value)
        sub(/"$/, "", value)
        print value
        exit
      }
    }
  ' "$root_manifest"
}

emit_cargo_crate_paths() {
  local manifest="$1"
  local visited_file="$2"
  local crate_dir dependency_rel dependency_dir dependency_name root_manifest root_dir
  if ! crate_dir="$(resolve_dir "$(dirname "$manifest")")"; then
    return
  fi
  manifest="${crate_dir}/Cargo.toml"
  if [[ ! -f "$manifest" ]] || grep -Fqx -- "$manifest" "$visited_file"; then
    return
  fi
  printf '%s\n' "$manifest" >>"$visited_file"
  printf '%s\n' "$manifest"
  if [[ -d "${crate_dir}/src" ]]; then
    printf '%s\n' "${crate_dir}/src"
  fi
  while IFS= read -r dependency_rel; do
    [[ -n "$dependency_rel" ]] || continue
    if dependency_dir="$(resolve_dir "${crate_dir}/${dependency_rel}")"; then
      emit_cargo_crate_paths "${dependency_dir}/Cargo.toml" "$visited_file"
    fi
  done < <(cargo_path_dependencies "$manifest")
  root_manifest="$(cargo_workspace_root_manifest "$(dirname "$crate_dir")")"
  if [[ -z "$root_manifest" ]]; then
    return 0
  fi
  root_dir="$(dirname "$root_manifest")"
  while IFS= read -r dependency_name; do
    [[ -n "$dependency_name" ]] || continue
    dependency_rel="$(cargo_workspace_dependency_path "$root_manifest" "$dependency_name")"
    [[ -n "$dependency_rel" ]] || continue
    if dependency_dir="$(resolve_dir "${root_dir}/${dependency_rel}")"; then
      emit_cargo_crate_paths "${dependency_dir}/Cargo.toml" "$visited_file"
    fi
  done < <(cargo_workspace_inherited_dependencies "$manifest")
}

discover_cargo_closure_paths() {
  local visited_file manifest
  visited_file="$(mktemp)"
  for manifest in "$@"; do
    emit_cargo_crate_paths "$manifest" "$visited_file"
  done
  rm -f "$visited_file"
}

android_module_source_sets() {
  local module_dir="$1"
  local source_root="${module_dir}/src"
  local source_set_dir source_set_name
  if [[ ! -d "$source_root" ]]; then
    return
  fi
  while IFS= read -r source_set_dir; do
    source_set_name="$(basename "$source_set_dir")"
    case "$source_set_name" in
      test|androidTest) continue ;;
    esac
    printf '%s\n' "$source_set_dir"
  done < <(find "$source_root" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort)
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
  )
  local -a apple_exporter_paths=(
    "$repo_root/apps/apple/Waddle"
  )
  local path
  while IFS= read -r path; do
    apple_dependency_paths+=("$path")
    apple_exporter_paths+=("$path")
  done < <(discover_cargo_closure_paths \
    "$repo_root/server/crates/waddle-xmpp-client/Cargo.toml" \
    "$repo_root/server/crates/waddle-xmpp-core/Cargo.toml" \
    "$repo_root/server/crates/waddle-xmpp-client-ffi/Cargo.toml")
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
  )
  local -a android_exporter_paths=(
    "$repo_root/apps/android/build.gradle.kts"
    "$repo_root/apps/android/settings.gradle.kts"
    "$repo_root/apps/android/gradle.properties"
    "$repo_root/apps/android/app/src/main/AndroidManifest.xml"
  )
  local path module_dir source_set_dir
  while IFS= read -r path; do
    android_dependency_paths+=("$path")
    android_exporter_paths+=("$path")
    module_dir="$(dirname "$path")"
    while IFS= read -r source_set_dir; do
      android_dependency_paths+=("$source_set_dir")
      android_exporter_paths+=("$source_set_dir")
    done < <(android_module_source_sets "$module_dir")
  done < <(find "$repo_root/apps/android" -type f -name "build.gradle.kts" 2>/dev/null | sort)
  require_file "apps/android/build.gradle.kts"
  require_file "apps/android/app/build.gradle.kts"
  require_file "apps/android/core/client/build.gradle.kts"
  require_file "apps/android/settings.gradle.kts"
  require_file "apps/android/gradle/libs.versions.toml"
  require_file "server/crates/waddle-xmpp-client/Cargo.toml"
  require_file "server/crates/waddle-xmpp-core/Cargo.toml"
  require_file "server/crates/waddle-xmpp-client-ffi/Cargo.toml"
  while IFS= read -r path; do
    android_dependency_paths+=("$path")
    android_exporter_paths+=("$path")
  done < <(discover_cargo_closure_paths \
    "$repo_root/server/crates/waddle-xmpp-client/Cargo.toml" \
    "$repo_root/server/crates/waddle-xmpp-core/Cargo.toml" \
    "$repo_root/server/crates/waddle-xmpp-client-ffi/Cargo.toml")
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
  )
  local -a wasm_exporter_paths=(
    "$repo_root/server/wasm-pkg/waddle-xmpp-client-wasm"
  )
  require_file "server/crates/waddle-xmpp-client-wasm/Cargo.toml"
  require_file "server/crates/waddle-xmpp-client-wasm/src/events.rs"
  require_file "server/crates/waddle-xmpp-client-wasm/src/driver.rs"
  require_file "server/wasm-pkg/waddle-xmpp-client-wasm/waddle_xmpp_client_wasm.js"
  require_file "server/crates/waddle-xmpp-client/Cargo.toml"
  require_file "server/crates/waddle-xmpp-core/Cargo.toml"
  require_file "server/crates/waddle-xmpp-client-ffi/Cargo.toml"
  local path
  while IFS= read -r path; do
    wasm_dependency_paths+=("$path")
    wasm_exporter_paths+=("$path")
  done < <(discover_cargo_closure_paths \
    "$repo_root/server/crates/waddle-xmpp-client-wasm/Cargo.toml" \
    "$repo_root/server/crates/waddle-xmpp-client/Cargo.toml" \
    "$repo_root/server/crates/waddle-xmpp-core/Cargo.toml" \
    "$repo_root/server/crates/waddle-xmpp-client-ffi/Cargo.toml")
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
