#!/usr/bin/env bash
# Runs after the upload finalizer. Link this run's artifact area, including on a
# run where an independent check failed; an APK is never evidence of all checks.
set -euo pipefail

android_apk_path="apps/android/app/build/outputs/apk/foss/debug/app-foss-debug.apk"
if [[ ! -s "$android_apk_path" ]]; then
  printf '%s\n' 'No Android APK was produced. Check the build logs; no download is available.' >> "${GITHUB_STEP_SUMMARY:?missing GITHUB_STEP_SUMMARY}"
  exit 1
fi

android_run_url="${GITHUB_SERVER_URL:?missing GITHUB_SERVER_URL}/${GITHUB_REPOSITORY:?missing GITHUB_REPOSITORY}/actions/runs/${GITHUB_RUN_ID:?missing GITHUB_RUN_ID}"
{
  printf '%s\n\n' '### Android APK'
  printf '[Download **waddle-android-apk** from this run](%s#artifacts).\n\n' "$android_run_url"
  printf '%s\n\n' 'Open the artifact, extract app-foss-debug.apk, and install it on Android 14 or later. GitHub sign-in is required to download Actions artifacts.'
  printf '%s\n' 'This is the FOSS debug build signed with the repository debug key. An APK download does not mean every check passed; review this run’s check results and upload step.'
} >> "${GITHUB_STEP_SUMMARY:?missing GITHUB_STEP_SUMMARY}"
