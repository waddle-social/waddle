<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callState,
  $lastCallError,
  clearCallState,
  reportCallError,
  sendMucCallLeave,
  tearDownActiveCall,
} from "@/lib/calls/call-store";
import { outboundCalls } from "@/lib/calls/outbound";
import { useCallEngine } from "@/lib/calls/use-call-engine";
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

// Connect to LiveKit as soon as the call transitions to `active` with
// a populated join. Disconnect on the way out. Re-runs whenever the
// active session id changes — guards against stale subscriptions
// when the user accepts a second call after the first ended.
watch(
  // Track sid + a snapshot of join — NOT the entire state object
  // — so unrelated `$callState.set` calls (e.g. setting media
  // flags or selfNick during teardown) don't trigger a reconnect.
  () =>
    state.value.phase === "active"
      ? { sid: state.value.sid, join: state.value.join, media: state.value.media }
      : null,
  async (active, _prev, onCleanup) => {
    if (!active) return;
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
  return (client?.xmpp as Parameters<typeof outboundCalls.sessionTerminate>[0] | undefined) ?? null;
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
  if (state.value.phase !== "active") {
    clearCallState();
    return;
  }
  const active = state.value;
  const sender = getSender();
  if (sender) {
    try {
      if (active.kind === "dm") {
        await outboundCalls.sessionTerminate(sender, active.peer, active.sid, "success");
      } else {
        await sendMucCallLeave(sender as unknown as Parameters<typeof sendMucCallLeave>[0], active.peer);
      }
    } catch (err) {
      reportCallError(err);
    }
  }
  await engine.disconnect();
  clearCallState();
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
  let target: HTMLVideoElement | null = null;
  const fromRemote = remoteTracks.value.find((t) => t.kind === "video");
  if (fromRemote) {
    target = (fromRemote.track as unknown as { attachedElements: HTMLMediaElement[] }).attachedElements.find(
      (el) => el instanceof HTMLVideoElement,
    ) as HTMLVideoElement | undefined ?? null;
  }
  if (!target) {
    const fromLocal = localTracks.value.find((t) => t.kind === "video");
    if (fromLocal) {
      target = (fromLocal.track as unknown as { attachedElements: HTMLMediaElement[] }).attachedElements.find(
        (el) => el instanceof HTMLVideoElement,
      ) as HTMLVideoElement | undefined ?? null;
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

onBeforeUnmount(() => {
  // Tab close mid-call: best-effort session-terminate so the peer's
  // overlay closes immediately instead of waiting for the LiveKit
  // room to drop them.
  void tearDownActiveCall(getSender(), "gone");
  void engine.disconnect();
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
