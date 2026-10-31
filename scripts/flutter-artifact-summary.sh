#!/usr/bin/env bash
set -euo pipefail
: "${GITHUB_STEP_SUMMARY:?GitHub summary path is required}"
{
  printf '%s\n\n' '### Alternative Flutter client'
  printf '%s\n\n' 'This is the independent social.waddle.flutter app. It installs alongside the Kotlin Android app. Artifacts expire after seven days.'
  if [[ -n "${FLUTTER_APK_URL:-}" ]]; then
    printf '[Download Flutter debug APK](%s) — unzip and install waddle-flutter.apk.\n\n' "$FLUTTER_APK_URL"
  fi
  if [[ -n "${FLUTTER_PREVIEWS_URL:-}" ]]; then
    printf '[Download rendered Flutter previews](%s)\n\n' "$FLUTTER_PREVIEWS_URL"
  fi
  if [[ -n "${FLUTTER_WEB_URL:-}" ]]; then
    printf '[Download Flutter web build](%s)\n\n' "$FLUTTER_WEB_URL"
  fi
  printf '%s\n' 'Artifact availability does not mean checks passed; review this run’s verification results.'
} >> "$GITHUB_STEP_SUMMARY"
