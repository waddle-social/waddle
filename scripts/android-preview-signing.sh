#!/usr/bin/env bash
# Called only on trusted main-branch CI; never print signing material.
set -euo pipefail

case "${1:-}" in
  prepare)
    : "${RUNNER_TEMP:?}" "${GITHUB_ENV:?}"
    : "${WADDLE_ANDROID_PREVIEW_KEYSTORE_BASE64:?missing preview keystore secret}"
    : "${WADDLE_ANDROID_PREVIEW_STORE_PASSWORD:?missing preview password secret}"
    umask 077
    signing_dir=$(mktemp -d "${RUNNER_TEMP}/waddle-preview-signing.XXXXXX")
    trap 'rm -rf -- "$signing_dir"' ERR
    printf '%s' "$WADDLE_ANDROID_PREVIEW_KEYSTORE_BASE64" | base64 --decode > "$signing_dir/preview.p12"
    printf '%s' "$WADDLE_ANDROID_PREVIEW_STORE_PASSWORD" > "$signing_dir/store-password"
    keytool -list -keystore "$signing_dir/preview.p12" -storetype PKCS12 \
      -storepass:env WADDLE_ANDROID_PREVIEW_STORE_PASSWORD -alias waddle-preview >/dev/null
    printf 'WADDLE_ANDROID_PREVIEW_SIGNING_DIR=%s\n' "$signing_dir" >> "$GITHUB_ENV"
    ;;
  cleanup)
    if [[ -n "${WADDLE_ANDROID_PREVIEW_SIGNING_DIR:-}" ]]; then
      case "$WADDLE_ANDROID_PREVIEW_SIGNING_DIR" in
        "${RUNNER_TEMP:?}"/waddle-preview-signing.*) rm -rf -- "$WADDLE_ANDROID_PREVIEW_SIGNING_DIR" ;;
        *) echo 'Refusing to clean an unexpected signing directory' >&2; exit 1 ;;
      esac
    fi
    ;;
  *) echo 'Usage: android-preview-signing.sh prepare|cleanup' >&2; exit 1 ;;
esac
