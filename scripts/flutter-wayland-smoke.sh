#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo/apps/flutter"
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

# Give the compositor and keyring their own session, independent of any desktop
# account, credentials or display available on the build machine.
unset DISPLAY WAYLAND_SOCKET GNOME_KEYRING_CONTROL WADDLE_FLUTTER_RUNTIME_LIBRARY
export XDG_RUNTIME_DIR="$scratch/run" XDG_DATA_HOME="$scratch/data"
export XDG_CONFIG_HOME="$scratch/config" XDG_CACHE_HOME="$scratch/cache"
export GDK_BACKEND=wayland WAYLAND_DISPLAY=waddle-smoke LIBGL_ALWAYS_SOFTWARE=1
mkdir -m 700 "$XDG_RUNTIME_DIR" "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"
dbus-run-session -- bash -eu -o pipefail <<'SESSION'
  weston --no-config --backend=headless --renderer=pixman --socket="$WAYLAND_DISPLAY" --idle-time=0 > "$XDG_RUNTIME_DIR/weston.log" 2>&1 &
  compositor=$!
  trap 'kill "$compositor" 2>/dev/null || true' EXIT
  for ((attempt=0; attempt<100; attempt++)); do
    [[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]] && break
    if ! kill -0 "$compositor" 2>/dev/null; then
      cat "$XDG_RUNTIME_DIR/weston.log" >&2
      exit 1
    fi
    sleep 0.1
  done
  [[ -S "$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY" ]]
  printf %s disposable-smoke-password | gnome-keyring-daemon --unlock --components=secrets
  flutter test --no-pub -d linux integration_test/standalone_smoke_test.dart
SESSION
