#!/usr/bin/env bash
# Boots a disposable emulator for the independent Flutter application's real plugins and Rust ABI.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ANDROID_HOME="${ANDROID_HOME:-$HOME/.android-sdk}"
export ANDROID_HOME
export ANDROID_SDK_ROOT="$ANDROID_HOME"
SDKMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/sdkmanager"
AVDMANAGER="$ANDROID_HOME/cmdline-tools/latest/bin/avdmanager"
ADB="$ANDROID_HOME/platform-tools/adb"
EMULATOR="$ANDROID_HOME/emulator/emulator"
SYSTEM_IMAGE='system-images;android-35;google_atd;x86_64'
SERIAL='emulator-5554'
SMOKE_TEMP="$(mktemp -d)"
SMOKE_LOG="$SMOKE_TEMP/emulator.log"
SMOKE_ARTIFACT="$REPO_ROOT/apps/flutter/build/device-smoke"
mkdir -p "$SMOKE_ARTIFACT"
EMULATOR_PID=''
SMOKE_BOOTED=false
CAPTURE_PID=''
SMOKE_CAPTURE_TOKEN="$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')"

capture_production_ui() {
  # This child must not run the parent's emulator cleanup trap.
  trap - EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  local capture_deadline=$((SECONDS + ${1:-600}))
  local ready_token=''
  local capture_file="$SMOKE_ARTIFACT/production.pending.png"
  while [[ $SECONDS -lt $capture_deadline ]]; do
    if ready_token="$(timeout 5 "$ADB" -s "$SERIAL" exec-out run-as social.waddle.flutter \
      cat cache/standalone-smoke.ready 2>/dev/null)" && [[ "$ready_token" == "$SMOKE_CAPTURE_TOKEN" ]]; then
      timeout 15 "$ADB" -s "$SERIAL" exec-out screencap -p > "$capture_file"
      python3 - "$capture_file" <<'PNG'
from pathlib import Path
import struct
import sys
image = Path(sys.argv[1]).read_bytes()
if (len(image) < 45 or image[:8] != b"\x89PNG\r\n\x1a\n" or image[12:16] != b"IHDR"
        or image[-12:] != b"\x00\x00\x00\x00IEND\xaeB`\x82"
        or min(struct.unpack(">II", image[16:24])) == 0):
    raise SystemExit("ADB did not return a complete PNG screenshot")
PNG
      mv "$capture_file" "$SMOKE_ARTIFACT/production.png"
      printf '%s' "$SMOKE_CAPTURE_TOKEN" | timeout 10 "$ADB" -s "$SERIAL" exec-in \
        run-as social.waddle.flutter tee cache/standalone-smoke.ack > /dev/null
      echo 'Captured mounted production UI and acknowledged its current marker.'
      return 0
    fi
    sleep 1
  done
  echo 'The production UI did not request its screenshot before the capture deadline.' >&2
  return 1
}

cleanup() {
  local smoke_status=$?
  trap - EXIT INT TERM
  if [[ -n "$CAPTURE_PID" ]]; then
    kill "$CAPTURE_PID" 2>/dev/null || true
    wait "$CAPTURE_PID" 2>/dev/null || true
  fi
  rm -f "$SMOKE_ARTIFACT/production.pending.png"
  printf 'exit_status=%s\n' "$smoke_status" > "$SMOKE_ARTIFACT/status.txt"
  if [[ -f "$SMOKE_LOG" ]]; then
    cp "$SMOKE_LOG" "$SMOKE_ARTIFACT/emulator.log"
  fi
  if [[ -n "$EMULATOR_PID" ]] && kill -0 "$EMULATOR_PID" 2>/dev/null; then
    if [[ "$SMOKE_BOOTED" == true ]]; then
      timeout 10 "$ADB" -s "$SERIAL" logcat -d -v threadtime > "$SMOKE_ARTIFACT/logcat.txt" 2>&1 || true
      timeout 10 "$ADB" -s "$SERIAL" exec-out screencap -p > "$SMOKE_ARTIFACT/emulator-screen.png" 2>/dev/null || true
      for smoke_image in emulator-screen.png production.png; do
        if [[ ! -s "$SMOKE_ARTIFACT/$smoke_image" ]]; then
          rm -f "$SMOKE_ARTIFACT/$smoke_image"
        fi
      done
      timeout 10 "$ADB" -s "$SERIAL" emu kill >/dev/null 2>&1 || true
    fi
    kill "$EMULATOR_PID" 2>/dev/null || true
    for smoke_attempt in {1..10}; do
      if ! kill -0 "$EMULATOR_PID" 2>/dev/null; then break; fi
      sleep 1
    done
    kill -KILL "$EMULATOR_PID" 2>/dev/null || true
    wait "$EMULATOR_PID" 2>/dev/null || true
  fi
  if [[ "$smoke_status" -ne 0 && -f "$SMOKE_LOG" ]]; then
    tail -n 160 "$SMOKE_LOG" >&2
  fi
  rm -rf "$SMOKE_TEMP"
  exit "$smoke_status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  echo 'The device smoke runner requires Linux x86_64 with KVM.' >&2
  exit 2
fi
if [[ ! -x "$SDKMANAGER" || ! -x "$AVDMANAGER" ]]; then
  echo 'Run scripts/setup-flutter-android.sh to provision the standalone SDK first.' >&2
  exit 1
fi
if [[ ! -r /dev/kvm || ! -w /dev/kvm ]]; then
  echo 'The ephemeral CI runner must grant its current user access to /dev/kvm.' >&2
  exit 1
fi
if [[ ! -f "$REPO_ROOT/apps/flutter/android/app/src/main/jniLibs/x86_64/libwaddle_flutter_runtime.so" ]]; then
  echo 'Run scripts/build-flutter-rust.sh --android before the device smoke test.' >&2
  exit 1
fi

# SDK paths and the AVD are independent of apps/android and any developer AVDs.
"$SDKMANAGER" --sdk_root="$ANDROID_HOME" 'platform-tools' 'emulator' "$SYSTEM_IMAGE"
export ANDROID_AVD_HOME="$SMOKE_TEMP/avds"
mkdir -p "$ANDROID_AVD_HOME"
printf 'no\n' | "$AVDMANAGER" create avd --force --name waddle-flutter-smoke \
  --package "$SYSTEM_IMAGE" --device pixel_2 --path "$SMOKE_TEMP/avd"

"$ADB" start-server
if "$ADB" -s "$SERIAL" get-state >/dev/null 2>&1; then
  echo "$SERIAL is already in use; refusing to replace another emulator." >&2
  exit 1
fi
"$EMULATOR" -avd waddle-flutter-smoke -port 5554 -no-window -no-audio \
  -no-snapshot -gpu swiftshader_indirect -no-boot-anim -wipe-data -accel on \
  -memory 2048 -cores 2 >"$SMOKE_LOG" 2>&1 &
EMULATOR_PID=$!

SMOKE_DEADLINE=$((SECONDS + 240))
while [[ $SECONDS -lt $SMOKE_DEADLINE ]]; do
  if ! kill -0 "$EMULATOR_PID" 2>/dev/null; then
    echo 'The emulator exited before completing boot.' >&2
    exit 1
  fi
  if [[ "$(timeout 10 "$ADB" -s "$SERIAL" shell getprop sys.boot_completed 2>/dev/null | tr -d '\r')" == 1 ]]; then
    SMOKE_BOOTED=true
    break
  fi
  sleep 2
done
if [[ "$SMOKE_BOOTED" != true ]]; then
  echo 'The emulator did not finish booting within 240 seconds.' >&2
  exit 1
fi
"$ADB" -s "$SERIAL" shell input keyevent 82
"$ADB" -s "$SERIAL" shell settings put global window_animation_scale 0
"$ADB" -s "$SERIAL" shell settings put global transition_animation_scale 0
"$ADB" -s "$SERIAL" shell settings put global animator_duration_scale 0

cd "$REPO_ROOT/apps/flutter"
# Capture is acknowledged only after the current app marker and a complete PNG.
rm -f "$SMOKE_ARTIFACT/production.png" "$SMOKE_ARTIFACT/production.pending.png"
capture_production_ui > "$SMOKE_ARTIFACT/capture.log" 2>&1 &
CAPTURE_PID=$!
# Desktop test overrides must never hide a missing Android-packaged library.
timeout 600 env -u WADDLE_FLUTTER_RUNTIME_LIBRARY flutter test \
  integration_test/standalone_smoke_test.dart -d "$SERIAL" --no-pub --reporter expanded \
  --dart-define="WADDLE_SMOKE_CAPTURE_TOKEN=$SMOKE_CAPTURE_TOKEN"
if wait "$CAPTURE_PID"; then
  CAPTURE_PID=''
else
  CAPTURE_PID=''
  echo 'The production UI screenshot was not captured and acknowledged.' >&2
  exit 1
fi
