<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, provide, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callState,
  $lastCallError,
  clearCallState,
  reportCallError,
  tearDownActiveCall,
} from "@/lib/calls/call-store";
import type { CallWireSender } from "@/lib/calls/outbound";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import {
  callVideoRegistryKey,
  createCallVideoRegistry,
} from "@/lib/calls/video-registry";
import { connectionStore } from "@/lib/connection-store";
import {
  $callUiMode,
  nonPipFallbackMode,
  rememberNonPipMode,
  type CallUiMode,
} from "@/lib/calls/ui-mode";
import CallTileGrid from "./CallTileGrid.vue";
import CallControls from "./CallControls.vue";
import CallFloatingWindow from "./CallFloatingWindow.vue";
import CallMinimizedChip from "./CallMinimizedChip.vue";
import CallSettingsDialog from "./CallSettingsDialog.vue";

/**
 * Thin shell — routes the active call between docked / floating /
 * minimized / pip surfaces. Engine wiring (connect/disconnect, mic
 * + cam toggles, hangup, tab-close cleanup) is owned here so each
 * surface stays free of lifecycle plumbing.
 *
 * The previous implementation was a single 320-line component with
 * a hardcoded dock-right layout. The user-reported pain — "overlay
 * blocks the view, can't be popped out, can't change settings" — was
 * a direct consequence of that monolith. Splitting the presentation
 * surfaces lets each one own only its concern: tile grid, drag
 * window, chip, controls, settings popover.
 */
const state = useStore($callState);
const lastError = useStore($lastCallError);
const uiMode = useStore($callUiMode);
const { engine, remoteTracks, localTracks } = useCallEngine();

const connecting = ref(false);
const micEnabled = ref(true);
const camEnabled = ref(true);
const settingsOpen = ref(false);
const pipVideoEl = ref<HTMLVideoElement | null>(null);

// Shared registry of `<video>` elements currently attached to call
// tracks. CallTileGrid populates it through its `:ref` callbacks; the
// PIP toggle below reads it instead of reaching into LiveKit's
// private `attachedElements` array on a `RemoteTrack` / `LocalTrack`.
const videoRegistry = createCallVideoRegistry();
provide(callVideoRegistryKey, videoRegistry);

/** Effective mode for rendering — pip falls back to the stored
 *  non-pip mode while still in PIP, because PIP isn't a "surface" we
 *  paint inline. */
const effectiveMode = computed<Exclude<CallUiMode, "pip">>(() => {
  if (uiMode.value === "pip") return nonPipFallbackMode();
  return uiMode.value;
});

watch(uiMode, (next) => {
  rememberNonPipMode(next);
});

// Connect to LiveKit as soon as the call transitions to `active`
// with a populated join. Disconnect on the way out. Tracks the
// session id as a PRIMITIVE — Vue's `watch` only re-fires when the
// returned value's reference changes, and constructing an object
// literal inside the source would re-trigger on every unrelated
// `$callState.set` (e.g. setting `selfNick` mid-teardown), which
// would tear down the engine right after it connected. That was the
// "publishing track → disconnect from room" cycle in the
// LiveKit-client logs. Keying on `sid` alone means: a fresh call
// with a new sid reconnects; status pokes don't.
watch(
  () => (state.value.phase === "active" ? state.value.sid : null),
  async (sid, _prev, onCleanup) => {
    if (!sid) return;
    // Re-read the call state at fire time; the snapshot we use for
    // the LiveKit connect is the one that was current when the sid
    // was assigned.
    const active = state.value;
    if (active.phase !== "active" || active.sid !== sid) return;
    connecting.value = true;
    try {
      await engine.connect(active.join, active.media);
      micEnabled.value = active.media.audio;
      camEnabled.value = active.media.video;
    } catch {
      // The server already verified the JWT before forwarding the
      // transport, so the most common failure here is the browser
      // denying mic/cam permissions. Hang up locally; the peer will
      // see session-terminate via the hangup path below.
      clearCallState();
    } finally {
      connecting.value = false;
    }
    onCleanup(() => {
      void engine.disconnect();
    });
  },
  { immediate: true },
);

function getSender() {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as CallWireSender | undefined) ?? null;
}

async function toggleMic(): Promise<void> {
  micEnabled.value = !micEnabled.value;
  try {
    await engine.setMicEnabled(micEnabled.value);
  } catch (err) {
    micEnabled.value = !micEnabled.value;
    reportCallError(err);
  }
}

async function toggleCam(): Promise<void> {
  camEnabled.value = !camEnabled.value;
  try {
    await engine.setCameraEnabled(camEnabled.value);
  } catch (err) {
    camEnabled.value = !camEnabled.value;
    reportCallError(err);
  }
}

async function hangup(): Promise<void> {
  // `tearDownActiveCall` emits the appropriate XEP-0166 /
  // urn:waddle:muc-call:0 stanza for the current phase AND, for MUC
  // calls, clears the `<call state="active"/>` presence advertisement
  // so neither the user's own client nor other occupants keep
  // rendering the room as "in call" after we leave. Previously the
  // hangup path sent `<request-leave/>` but skipped the presence
  // cleanup, leaving the "N in call" indicator visible until the XMPP
  // session disconnected — which read as "the call won't stop".
  await tearDownActiveCall(getSender(), "success");
  await engine.disconnect();
}

const peerLabel = computed(() => {
  if (state.value.phase !== "active") return "";
  const at = state.value.peer.indexOf("@");
  const localpart = at > 0 ? state.value.peer.slice(0, at) : state.value.peer;
  return state.value.kind === "muc" ? `#${localpart}` : localpart;
});

const subline = computed(() => {
  if (state.value.phase !== "active") return "";
  if (connecting.value) return "Connecting…";
  if (state.value.kind === "muc") {
    const others = remoteParticipantCount.value;
    return others === 0
      ? "Waiting for others to join…"
      : `${others} other${others === 1 ? "" : "s"} in call`;
  }
  return state.value.media.video ? "Video call" : "Audio call";
});

const remoteParticipantCount = computed(() => {
  const ids = new Set<string>();
  for (const t of remoteTracks.value) ids.add(t.participantIdentity);
  return ids.size;
});

const pipSupported = computed(() => {
  if (typeof document === "undefined") return false;
  return Boolean(document.pictureInPictureEnabled);
});

const hasVideoForPip = computed(() => {
  return remoteTracks.value.some((t) => t.kind === "video")
    || localTracks.value.some((t) => t.kind === "video");
});

function setMode(mode: CallUiMode): void {
  $callUiMode.set(mode);
}

async function togglePip(): Promise<void> {
  if (typeof document === "undefined" || !document.pictureInPictureEnabled) return;
  // Prefer the first remote video; fall back to a local one only if
  // no remote video exists (1:1 self-view PIP for self-debugging).
  // Resolved via the shared registry that CallTileGrid populates on
  // every `<video>` attach — no LiveKit-private API access.
  let target: HTMLVideoElement | null = null;
  const fromRemote = remoteTracks.value.find((t) => t.kind === "video");
  if (fromRemote) {
    target = videoRegistry.get(fromRemote.participantIdentity);
  }
  if (!target) {
    const fromLocal = localTracks.value.find((t) => t.kind === "video");
    if (fromLocal) {
      target = videoRegistry.get(fromLocal.participantIdentity);
    }
  }
  if (!target) return;
  try {
    if (document.pictureInPictureElement) {
      await document.exitPictureInPicture();
      setMode(nonPipFallbackMode());
    } else {
      await target.requestPictureInPicture();
      pipVideoEl.value = target;
      setMode("pip");
      target.addEventListener(
        "leavepictureinpicture",
        () => {
          if (uiMode.value === "pip") setMode(nonPipFallbackMode());
          pipVideoEl.value = null;
        },
        { once: true },
      );
    }
  } catch (err) {
    reportCallError(err);
  }
}

/**
 * Best-effort teardown for the tab-close path.
 *
 * `onBeforeUnmount` runs too late for tab close — the browser will
 * tear the WebSocket down before our async wire-send completes.
 * `pagehide` fires earlier in the unload sequence and is the modern
 * documented hook for unload-side networking.
 *
 * IMPORTANT: this is best-effort, not a guarantee. `tearDownActiveCall`
 * awaits an actor round-trip for stanza acknowledgement, and the
 * browser will not yield long enough for the full chain to complete
 * before the WebSocket is torn down. Some of the wire frames will
 * make it (typically the first synchronous send), some may not.
 *
 * The authoritative chip-clear path is server-side: the unclean
 * disconnect handler (`cleanup_muc_presence`) now broadcasts an
 * `unavailable` presence to remaining occupants regardless of whether
 * this client managed to send a graceful `state='inactive'` first.
 * This handler is here to shorten the gap, not to be relied on.
 */
function handlePageHide(): void {
  const sender = getSender();
  if (!sender) return;
  void tearDownActiveCall(sender, "gone");
}

onMounted(() => {
  if (typeof window !== "undefined") {
    window.addEventListener("pagehide", handlePageHide);
  }
});

onBeforeUnmount(() => {
  // Tab close mid-call: best-effort session-terminate so the peer's
  // overlay closes immediately instead of waiting for the LiveKit
  // room to drop them. The `pagehide` handler above is the primary
  // route on actual tab-close; this branch handles in-app overlay
  // unmounts (e.g. route change away from a call surface).
  void tearDownActiveCall(getSender(), "gone");
  void engine.disconnect();
  // Exit PIP if our video was the one in the floating window —
  // otherwise the browser leaves a frozen still-frame window visible
  // after the call has ended, confusing the user about whether the
  // call is still running.
  if (
    typeof document !== "undefined" &&
    pipVideoEl.value &&
    document.pictureInPictureElement === pipVideoEl.value
  ) {
    void document.exitPictureInPicture().catch(() => undefined);
  }
  pipVideoEl.value = null;
  if (typeof window !== "undefined") {
    window.removeEventListener("pagehide", handlePageHide);
  }
});

const localIdentity = computed(() => engine.localIdentity);
</script>

<template>
  <template v-if="state.phase === 'active'">
    <!-- Minimized: small chip in the corner. Doesn't render the grid
         or settings dialog (settings can still be opened by restoring). -->
    <CallMinimizedChip
      v-if="effectiveMode === 'minimized'"
      :label="peerLabel"
      :connecting="connecting"
      @restore="setMode('docked')"
      @hangup="hangup"
    />

    <!-- Floating window: draggable, smaller, leaves the channel pane
         full-width behind it. -->
    <CallFloatingWindow v-else-if="effectiveMode === 'floating'">
      <template #header>
        <div class="min-w-0 flex-1">
          <div class="type-chat-title truncate">{{ peerLabel }}</div>
          <div class="type-caption text-muted-foreground truncate">{{ subline }}</div>
        </div>
      </template>
      <div
        v-if="lastError"
        class="border-b border-destructive/30 bg-destructive/10 px-3 py-2 type-caption text-destructive"
        role="alert"
      >
        {{ lastError }}
      </div>
      <CallTileGrid
        :remote-tracks="remoteTracks"
        :local-tracks="localTracks"
        :local-identity="localIdentity"
        :mic-enabled="micEnabled"
      />
      <template #footer>
        <CallControls
          :mic-enabled="micEnabled"
          :cam-enabled="camEnabled"
          :pip-supported="pipSupported"
          :has-video-for-pip="hasVideoForPip"
          :mode="uiMode"
          @toggle-mic="toggleMic"
          @toggle-cam="toggleCam"
          @toggle-pip="togglePip"
          @set-mode="setMode"
          @open-settings="settingsOpen = true"
          @hangup="hangup"
        />
      </template>
    </CallFloatingWindow>

    <!-- Docked panel (default): full-height right-edge dock on
         desktop, fullscreen takeover on mobile. -->
    <aside
      v-else
      class="call-docked-panel"
      role="dialog"
      aria-label="Active call"
    >
      <header class="call-docked-panel__header">
        <div class="min-w-0 flex-1">
          <div class="type-chat-title truncate">{{ peerLabel }}</div>
          <div class="type-caption text-muted-foreground truncate">{{ subline }}</div>
        </div>
      </header>

      <div
        v-if="lastError"
        class="call-docked-panel__error"
        role="alert"
      >
        {{ lastError }}
      </div>

      <CallTileGrid
        :remote-tracks="remoteTracks"
        :local-tracks="localTracks"
        :local-identity="localIdentity"
        :mic-enabled="micEnabled"
      />

      <footer class="call-docked-panel__footer">
        <CallControls
          :mic-enabled="micEnabled"
          :cam-enabled="camEnabled"
          :pip-supported="pipSupported"
          :has-video-for-pip="hasVideoForPip"
          :mode="uiMode"
          @toggle-mic="toggleMic"
          @toggle-cam="toggleCam"
          @toggle-pip="togglePip"
          @set-mode="setMode"
          @open-settings="settingsOpen = true"
          @hangup="hangup"
        />
      </footer>
    </aside>

    <CallSettingsDialog v-model:open="settingsOpen" />
  </template>
</template>

<style scoped>
.call-docked-panel {
  position: fixed;
  inset: 0;
  z-index: 40;
  display: flex;
  flex-direction: column;
  background: color-mix(in oklab, var(--background) 95%, transparent);
  backdrop-filter: blur(12px);
}

@media (min-width: 768px) {
  .call-docked-panel {
    inset: 0 0 0 auto;
    width: 22rem;
    border-left: 1px solid var(--border);
  }
}

@media (min-width: 1024px) {
  .call-docked-panel {
    width: 26rem;
  }
}

.call-docked-panel__header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-sm);
  border-bottom: 1px solid var(--border);
  padding: 0.75rem 1rem;
}

.call-docked-panel__footer {
  display: flex;
  align-items: center;
  justify-content: center;
  border-top: 1px solid var(--border);
  padding: 0.75rem 1rem;
}

.call-docked-panel__error {
  border-bottom: 1px solid color-mix(in oklab, var(--destructive) 30%, transparent);
  background: color-mix(in oklab, var(--destructive) 10%, transparent);
  padding: 0.5rem 1rem;
  color: var(--destructive);
}
</style>
