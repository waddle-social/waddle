<script setup lang="ts">
import { computed, ref, toRef } from "vue";
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
  toggleCam,
  toggleMic,
} from "@/lib/calls/call-controls";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { useActiveMucCall } from "@/lib/calls/use-active-muc-call";
import { useSplitResize } from "@/lib/calls/use-split-resize";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import CallTileGrid from "./CallTileGrid.vue";
import CallControls from "./CallControls.vue";
import CallSettingsDialog from "./CallSettingsDialog.vue";
import SplitDragHandle from "./SplitDragHandle.vue";

/**
 * Inline split-view surface for an active MUC group call.
 *
 * Mounted by `ContentArea.vue` between the channel header/banners
 * and the message timeline. Renders only when ALL of the following
 * hold:
 *   - There is an active call in `$callState` with `kind === "muc"`.
 *   - The room hosting the call matches the channel currently being
 *     viewed (props.roomJid).
 *   - The local user's nick is in that room's Muji-active list.
 *   - `$callUiMode` is `"split"` (not `"fullscreen"`).
 *
 * The split lives ABOVE the timeline so the chat is always visible
 * beneath; the drag handle resizes the two regions inside the parent
 * flex column. See `use-split-resize.ts` for the clamp + persistence
 * contract.
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
const containerRef = ref<HTMLElement | null>(null);

const normalizedRoomJid = computed(() =>
  normalizeMucCallRoomJid(props.roomJid),
);

const ownsThisCall = computed(() => {
  return (
    activeRoomJid.value !== null &&
    activeRoomJid.value === normalizedRoomJid.value
  );
});

const shouldRender = computed(() => {
  return (
    state.value.phase === "active" &&
    state.value.kind === "muc" &&
    uiMode.value === "split" &&
    ownsThisCall.value &&
    selfInCall.value
  );
});

const roomJidRef = toRef(props, "roomJid");
const { isDragging, percent, beginDrag } = useSplitResize(roomJidRef);

function getColumnRect(): DOMRect | null {
  // The flex column we resize within is `containerRef`'s offsetParent
  // — that's the ChatContentArea flex column. The handle's pointer
  // event positions are relative to the viewport; the parent rect
  // turns them into a percentage of the available column height.
  const el = containerRef.value;
  if (!el) return null;
  const parent = el.parentElement;
  if (!parent) return null;
  return parent.getBoundingClientRect();
}

function onHandlePress(event: PointerEvent): void {
  beginDrag(event, getColumnRect);
}

function enterExpanded(): void {
  $callUiMode.set("expanded");
}

const remoteParticipantCount = computed(() => {
  const ids = new Set<string>();
  for (const t of remoteTracks.value) ids.add(t.participantIdentity);
  return ids.size;
});

const subline = computed(() => {
  if (state.value.phase !== "active") return "";
  if (connecting.value) return "Connecting…";
  const others = remoteParticipantCount.value;
  return others === 0
    ? "Waiting for others to join…"
    : `${others} other${others === 1 ? "" : "s"} in call`;
});

const localIdentity = computed(() => engine.localIdentity);

async function onHangup(): Promise<void> {
  await hangupActiveCall();
}
</script>

<template>
  <template v-if="shouldRender">
    <section
      ref="containerRef"
      class="call-split"
      :style="{ flexBasis: `${percent}%` }"
      :class="{ 'call-split--dragging': isDragging }"
      role="region"
      aria-label="Active call"
    >
      <header class="call-split__header">
        <div class="call-split__title">
          <span class="call-split__live-dot" aria-hidden="true" />
          <span class="type-control">Live · {{ subline }}</span>
        </div>
      </header>
      <div
        v-if="lastError"
        class="call-split__error"
        role="alert"
      >
        {{ lastError }}
      </div>
      <div class="call-split__grid">
        <CallTileGrid
          :remote-tracks="remoteTracks"
          :local-tracks="localTracks"
          :local-identity="localIdentity"
          :mic-enabled="micEnabled"
        />
      </div>
      <footer class="call-split__footer">
        <CallControls
          :mic-enabled="micEnabled"
          :cam-enabled="camEnabled"
          :is-expanded="false"
          @toggle-mic="toggleMic"
          @toggle-cam="toggleCam"
          @toggle-expanded="enterExpanded"
          @open-settings="settingsOpen = true"
          @hangup="onHangup"
        />
      </footer>
    </section>
    <SplitDragHandle
      :dragging="isDragging"
      :percent="percent"
      @press="onHandlePress"
    />
    <CallSettingsDialog v-model:open="settingsOpen" />
  </template>
</template>

<style scoped>
.call-split {
  position: relative;
  display: flex;
  flex-direction: column;
  flex-grow: 0;
  flex-shrink: 0;
  min-height: 0;
  background: color-mix(in oklab, var(--background) 94%, transparent);
  border-bottom: 1px solid var(--border);
  /* Smooth open animation when the split appears (call accepted): the
   * parent uses `transition: flex-basis` for the open/close grow but
   * during a drag we set `--call-split-dragging` to disable the
   * transition so the handle tracks the pointer 1:1. */
  transition: flex-basis 220ms ease-out;
}

.call-split--dragging {
  transition: none;
}

.call-split__header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-sm);
  padding: 0.5rem 0.75rem;
  border-bottom: 1px solid var(--border);
}

.call-split__title {
  display: inline-flex;
  align-items: center;
  gap: 0.5rem;
  color: var(--muted-foreground);
}

.call-split__live-dot {
  width: 0.5rem;
  height: 0.5rem;
  border-radius: 9999px;
  background: oklch(0.7 0.18 145);
  box-shadow: 0 0 0 4px color-mix(in oklab, oklch(0.7 0.18 145) 25%, transparent);
  animation: call-split-live-pulse 1.6s ease-in-out infinite;
}

@media (prefers-reduced-motion: reduce) {
  .call-split__live-dot {
    animation: none;
  }
}

@keyframes call-split-live-pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.55; }
}

.call-split__error {
  padding: 0.5rem 0.75rem;
  border-bottom: 1px solid color-mix(in oklab, var(--destructive) 30%, transparent);
  background: color-mix(in oklab, var(--destructive) 10%, transparent);
  color: var(--destructive);
}

.call-split__grid {
  flex: 1 1 0%;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.call-split__footer {
  padding: 0.5rem 0.75rem;
  border-top: 1px solid var(--border);
}
</style>
