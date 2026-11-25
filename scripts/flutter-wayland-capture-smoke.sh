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
if [[ -n "${WADDLE_LIVEKIT_FIXTURE:-}" ]]; then
  gst=$(nix build --no-link --print-out-paths --inputs-from . 'nixpkgs#gst_all_1.gstreamer^bin')
  gst_core=$(tool gst_all_1.gstreamer)
  gst_base=$(tool gst_all_1.gst-plugins-base)
  espeak=$(tool espeak-ng)
  export gst gst_core gst_base espeak
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export XDG_RUNTIME_DIR="$work/run" XDG_CONFIG_HOME="$work/config" XDG_DATA_HOME="$work/data" XDG_CACHE_HOME="$work/cache"
export XDG_CURRENT_DESKTOP=sway XDG_SESSION_TYPE=wayland GDK_BACKEND=wayland
export XDG_DESKTOP_PORTAL_DIR="$wlr/share/xdg-desktop-portal/portals"
unset DISPLAY WAYLAND_DISPLAY WAYLAND_SOCKET SWAYSOCK PIPEWIRE_REMOTE PULSE_SERVER GNOME_KEYRING_CONTROL
mkdir -m 700 "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME" "$XDG_DATA_HOME" "$XDG_CACHE_HOME"
mkdir "$XDG_CONFIG_HOME/xdg-desktop-portal-wlr" "$XDG_CONFIG_HOME/xdg-desktop-portal"
# WebRTC maps four V4L2 capture buffers. Keep the private camera producer
# and the PipeWire V4L2 adapter on the same four-buffer queue.
mkdir -p "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d"
echo 'context.properties = { link.max-buffers = 4 }' > "$XDG_CONFIG_HOME/pipewire/pipewire.conf.d/50-camera.conf"
mkdir -p "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d"
cat > "$XDG_CONFIG_HOME/wireplumber/wireplumber.conf.d/50-isolate.conf" <<'CONFIG'
wireplumber.profiles = { main = {
  monitor.alsa = disabled
  monitor.bluez = disabled
  monitor.v4l2 = disabled
  monitor.libcamera = disabled
} }
CONFIG
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
  export PULSE_SINK=waddle_capture_test
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
    # A single packed plane can be shared by GStreamer and the PipeWire V4L2
    # adapter used by WebRTC. A synthetic camera sees no host camera devices.
    export GST_PLUGIN_PATH="$gst_core/lib/gstreamer-1.0:$gst_base/lib/gstreamer-1.0:$pipewire/lib/gstreamer-1.0"
    "$gst/bin/gst-launch-1.0" -q videotestsrc is-live=true pattern=snow \
      ! video/x-raw,format=YUY2,width=1280,height=720,framerate=30/1 \
      ! pipewiresink mode=provide stream-properties='props,media.class=Video/Source,node.name=waddle-camera-test,node.description=WaddleTestCamera' \
      >"$XDG_RUNTIME_DIR/camera.log" 2>&1 &
    for ((attempt=0; attempt<100; attempt++)); do
      nodes=$("$pipewire/bin/pw-cli" ls Node)
      [[ "$nodes" == *waddle-camera-test* ]] && break
      sleep 0.1
    done
    [[ "$nodes" == *waddle-camera-test* ]] || { cat "$XDG_RUNTIME_DIR/camera.log" >&2; exit 1; }
    # The fixture injects speech/noise into a dedicated source. Separate remote
    # playout prevents the decoded participant's audio feeding back into capture.
    "$pulse/bin/pactl" load-module module-null-sink sink_name=waddle_receiver_output >/dev/null
    "$pulse/bin/pactl" load-module module-remap-source master=waddle_capture_test.monitor source_name=waddle_test_mic >/dev/null
    "$pulse/bin/pactl" load-module module-null-sink sink_name=waddle_second_input >/dev/null
    "$pulse/bin/pactl" load-module module-remap-source master=waddle_second_input.monitor \
      source_name=waddle_second_mic source_properties=device.description=WaddleSecondMic >/dev/null
    export WADDLE_TEST_SECOND_MIC=WaddleSecondMic
    "$pulse/bin/pactl" set-default-source waddle_test_mic
    "$pulse/bin/pactl" set-default-sink waddle_receiver_output
    export PULSE_SOURCE=waddle_test_mic PULSE_SINK=waddle_receiver_output
    export WADDLE_TEST_SPEECH="$XDG_RUNTIME_DIR/speech.wav"
    "$espeak/bin/espeak-ng" -w "$WADDLE_TEST_SPEECH" 'The weather is clear and sunny beside the river.'
    export WADDLE_ISOLATED_CAMERA=1
    tests+=(integration_test/wayland_sfu_test.dart integration_test/wayland_microphone_filters_test.dart)
  fi
  for test in "${tests[@]}"; do
    run=()
    [[ "$test" == integration_test/wayland_sfu_test.dart ]] && run=("$pipewire/bin/pw-v4l2")
    "${run[@]}" flutter test --no-pub -d linux "$test" || {
      cat "$XDG_RUNTIME_DIR/"{portal,wlr,wireplumber}.log >&2
      exit 1
    }
  done
SESSION
