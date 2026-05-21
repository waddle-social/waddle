<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callState,
  $lastCallError,
} from "@/lib/calls/call-store";
import { $callUiMode } from "@/lib/calls/ui-mode";
import {
  $callConnecting,
  $callMicEnabled,
  $callCamEnabled,
  hangupActiveCall,
  peerLabelFromState,
  toggleCam,
  toggleMic,
} from "@/lib/calls/call-controls";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { useActiveMucCall } from "@/lib/calls/use-active-muc-call";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import CallTileGrid from "./CallTileGrid.vue";
import CallControls from "./CallControls.vue";
import CallSettingsDialog from "./CallSettingsDialog.vue";

/**
 * "Expanded" call surface — fills the chat content pane while the
 * surrounding app shell (left rails, sidebar, thread panel) stays
 * visible. Deliberately NOT the browser's native fullscreen: we want
 * the user to keep their bearings inside the app, not be pushed into
 * an immersive takeover.
 *
 * Mounted INSIDE `ContentArea.vue` as an absolutely-positioned child
 * of the area's `position: relative` root. When active, it covers the
 * header + banners + split + timeline + composer in one paint, so
 * the user sees only the call.
 *
 * Renders only when:
 *   - There is an active MUC call (`kind === "muc"`).
 *   - The call's room matches the channel currently being viewed.
 *   - The local user is a participant.
 *   - `$callUiMode === "expanded"`.
 */
const props = defineProps<{
  roomJid: string;
}>();

const state = useStore($callState);
const uiMode = useStore($callUiMode);
const lastError = useStore($lastCallError);
const connecting = useStore($callConnecting);
const micEnabled = useStore($callMicEnabled);
const camEnabled = useStore($callCamEnabled);
const { remoteTracks, localTracks, engine } = useCallEngine();
const { activeRoomJid, selfInCall } = useActiveMucCall();

const settingsOpen = ref(false);

const normalizedRoomJid = computed(() =>
  normalizeMucCallRoomJid(props.roomJid),
);

const ownsThisCall = computed(() => {
  return (
    activeRoomJid.value !== null &&
    activeRoomJid.value === normalizedRoomJid.value
  );
});

const isOpen = computed(() => {
  return (
    state.value.phase === "active" &&
    state.value.kind === "muc" &&
    uiMode.value === "expanded" &&
    ownsThisCall.value &&
    selfInCall.value
  );
});

const remoteParticipantCount = computed(() => {
  const ids = new Set<string>();
  for (const t of remoteTracks.value) ids.add(t.participantIdentity);
  return ids.size;
});

const peerLabel = computed(() => peerLabelFromState());

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

const localIdentity = computed(() => engine.localIdentity);

function collapseToSplit(): void {
  $callUiMode.set("split");
}

async function onHangup(): Promise<void> {
  await hangupActiveCall();
  // After hangup the surface unmounts on its own (phase flips out of
  // "active"); flip the mode back so the next call defaults to split
  // instead of inheriting "expanded".
  $callUiMode.set("split");
}

function onKeydown(event: KeyboardEvent): void {
  if (!isOpen.value) return;
  if (event.key === "Escape") {
    event.preventDefault();
    collapseToSplit();
  }
}

onMounted(() => {
  if (typeof window !== "undefined") {
    window.addEventListener("keydown", onKeydown);
  }
});

onBeforeUnmount(() => {
  if (typeof window !== "undefined") {
    window.removeEventListener("keydown", onKeydown);
  }
});
</script>

<template>
  <div
    v-if="isOpen"
    class="call-expanded"
    role="region"
    aria-label="Active call (expanded)"
  >
    <header class="call-expanded__header">
      <div class="min-w-0 flex-1">
        <div class="type-chat-title truncate">{{ peerLabel }}</div>
        <div class="type-caption text-muted-foreground truncate">{{ subline }}</div>
      </div>
    </header>
    <div
      v-if="lastError"
      class="call-expanded__error"
      role="alert"
    >
      {{ lastError }}
    </div>
    <main class="call-expanded__grid">
      <CallTileGrid
        :remote-tracks="remoteTracks"
        :local-tracks="localTracks"
        :local-identity="localIdentity"
        :mic-enabled="micEnabled"
      />
    </main>
    <footer class="call-expanded__footer">
      <CallControls
        :mic-enabled="micEnabled"
        :cam-enabled="camEnabled"
        :is-expanded="true"
        @toggle-mic="toggleMic"
        @toggle-cam="toggleCam"
        @toggle-expanded="collapseToSplit"
        @open-settings="settingsOpen = true"
        @hangup="onHangup"
      />
    </footer>
    <CallSettingsDialog v-model:open="settingsOpen" />
  </div>
</template>

<style scoped>
.call-expanded {
  /* Absolutely positioned within ContentArea's `position: relative`
   * root, so the call paints over the chat without escaping the app
   * shell. Sidebars, channel list, and thread panel stay visible. */
  position: absolute;
  inset: 0;
  z-index: 30;
  display: flex;
  flex-direction: column;
  background: var(--background);
}

.call-expanded__header {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: var(--space-sm);
  border-bottom: 1px solid var(--border);
  padding: 0.75rem 1rem;
}

.call-expanded__error {
  border-bottom: 1px solid color-mix(in oklab, var(--destructive) 30%, transparent);
  background: color-mix(in oklab, var(--destructive) 10%, transparent);
  padding: 0.5rem 1rem;
  color: var(--destructive);
}

.call-expanded__grid {
  flex: 1 1 0%;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.call-expanded__footer {
  display: flex;
  align-items: center;
  justify-content: center;
  border-top: 1px solid var(--border);
  padding: 0.75rem 1rem;
}
</style>
