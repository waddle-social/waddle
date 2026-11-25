#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if [[ -n "${WADDLE_LIVEKIT_FIXTURE:-}" ]]; then
  WADDLE_LIVEKIT_FIXTURE=$(realpath -e "$WADDLE_LIVEKIT_FIXTURE")
  export WADDLE_LIVEKIT_FIXTURE
fi
cd "$repo"
# Use the compositor and portal implementations from the project's pinned nixpkgs.
tool() { nix build --no-link --print-out-paths --inputs-from . "nixpkgs#$1^out"; }
sway=$(tool sway)
pipewire=$(tool pipewire)
wireplumber=$(tool wireplumber)
pulse=$(tool pulseaudio)
portal=$(tool xdg-desktop-portal)
wlr=$(tool xdg-desktop-portal-wlr)
export sway pipewire wireplumber pulse portal wlr

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export XDG_RUNTIME_DIR="$work/run" XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data" XDG_CACHE_HOME="$work/cache"
export XDG_CURRENT_DESKTOP=sway XDG_SESSION_TYPE=wayland GDK_BACKEND=wayland
export XDG_DESKTOP_PORTAL_DIR="$wlr/share/xdg-desktop-portal/portals"
unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET SWAYSOCK PIPEWIRE_REMOTE PULSE_SERVER GNOME_KEYRING_CONTROL
mkdir -m 700 "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
mkdir "$XDG_CONFIG_HOME/xdg-desktop-portal-wlr" "$XDG_CONFIG_HOME/xdg-desktop-portal"
printf '[preferred]\ndefault=wlr\n' > "$XDG_CONFIG_HOME/xdg-desktop-portal/sway-portals.conf"
echo 'output * resolution 1280x720' > "$XDG_CONFIG_HOME/sway.conf"
# Only this private compositor's software output can be shared.
cat > "$XDG_CONFIG_HOME/xdg-desktop-portal-wlr/config" <<'CONFIG'
[screencast]
chooser_type=none
output_name=HEADLESS-1
CONFIG
cd apps/flutter
bus_config=()
if [[ -n "${WADDLE_DBUS_SESSION_CONFIG:-}" ]]; then
  bus_config=(--config-file="$WADDLE_DBUS_SESSION_CONFIG")
fi
dbus-run-session "${bus_config[@]}" -- bash -eu <<'SESSION'
  trap 'kill $(jobs -p) 2>/dev/null || true' EXIT
  WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS=1 WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 \
    "$sway/bin/sway" --unsupported-gpu -c "$XDG_CONFIG_HOME/sway.conf" >"$XDG_RUNTIME_DIR/sway.log" 2>&1 &
  for ((attempt=0; attempt<100; attempt++)); do
    for socket in "$XDG_RUNTIME_DIR"/wayland-*; do
      if [[ -S "$socket" ]]; then export WAYLAND_DISPLAY="${socket##*/}"; break 2; fi
    done
    sleep 0.1
  done
  : "${WAYLAND_DISPLAY:?The isolated Wayland compositor did not start}"
  # A separate animated client continues generating damage while the camera
  # plugin waits synchronously for its next captured frame on GTK's thread.
  weston-presentation-shm -f > /dev/null 2>"$XDG_RUNTIME_DIR/animation.log" &
  "$pipewire/bin/pipewire" >"$XDG_RUNTIME_DIR/pipewire.log" 2>&1 &
  "$pipewire/bin/pipewire-pulse" >"$XDG_RUNTIME_DIR/pulse.log" 2>&1 &
  "$wireplumber/bin/wireplumber" >"$XDG_RUNTIME_DIR/wireplumber.log" 2>&1 &
  export PULSE_SERVER="unix:$XDG_RUNTIME_DIR/pulse/native"
  for ((attempt=0; attempt<100; attempt++)); do
    "$pulse/bin/pactl" info >/dev/null 2>&1 && break
    sleep 0.1
  done
  "$pulse/bin/pactl" load-module module-null-sink sink_name=waddle_capture_test >/dev/null
  export PULSE_SOURCE=waddle_capture_test.monitor
  "$wlr/libexec/xdg-desktop-portal-wlr" -l DEBUG >"$XDG_RUNTIME_DIR/wlr.log" 2>&1 &
  "$portal/libexec/xdg-desktop-portal" >"$XDG_RUNTIME_DIR/portal.log" 2>&1 &
  for service in org.freedesktop.impl.portal.desktop.wlr org.freedesktop.portal.Desktop; do
    for ((attempt=0; attempt<100; attempt++)); do
      reply=$(dbus-send --session --dest=org.freedesktop.DBus --print-reply \
        /org/freedesktop/DBus org.freedesktop.DBus.NameHasOwner "string:$service")
      [[ "$reply" == *"boolean true"* ]] && break
      sleep 0.1
    done
    [[ "$reply" == *"boolean true"* ]] || { cat "$XDG_RUNTIME_DIR/"{portal,wlr}.log >&2; exit 1; }
  done
  export WADDLE_CAPTURE_TRACE="$XDG_RUNTIME_DIR/wlr.log" WADDLE_ISOLATED_CAPTURE=1
  tests=(integration_test/wayland_screen_capture_test.dart)
  if [[ -n "${WADDLE_LIVEKIT_FIXTURE:-}" ]]; then
    tests+=(integration_test/wayland_sfu_test.dart)
  fi
  for test in "${tests[@]}"; do
    flutter test --no-pub -d linux "$test" || {
      cat "$XDG_RUNTIME_DIR/"{portal,wlr,wireplumber}.log >&2
      exit 1
    }
  done
SESSION
