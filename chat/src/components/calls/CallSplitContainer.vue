<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callState,
  $lastCallError,
  reportCallError,
} from "@/lib/calls/call-store";
import { $callUiMode } from "@/lib/calls/ui-mode";
import {
  $callSelfViewHidden,
  $callPinnedTileKey,
  $callViewMode,
  setCallViewMode,
  toggleCallSelfViewHidden,
} from "@/lib/calls/call-view-state";
import {
  $callConnecting,
  $callMicEnabled,
  $callCamEnabled,
  $callScreenShareEnabled,
  $callScreenShareSupported,
  hangupActiveCall,
  peerLabelFromState,
  refreshScreenShareSupported,
  toggleCam,
  toggleMic,
  toggleScreenShare,
} from "@/lib/calls/call-controls";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { useActiveMucCall } from "@/lib/calls/use-active-muc-call";
import { useSplitResize } from "@/lib/calls/use-split-resize";
import { localScreenSharePresentation } from "@/lib/calls/call-self-share";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { useCallRoster } from "@/lib/calls/use-call-roster";
import { openCallDock, setCallDockTab } from "@/lib/calls/call-dock-state";
import { $callChatUnread } from "@/lib/calls/call-chat-unread";
import {
  $inCallReactions,
  activeInCallReactions,
  clearInCallReactions,
  expireInCallReactions,
  hashInCallReactionId,
  receiveInCallReaction,
} from "@/lib/calls/in-call-reactions";
import { barePeerJid, fullJidIdentityKey } from "@/lib/xmpp/jid";
import {
  $mucRaisedHands,
  $selfRaisedHand,
  raisedHandKeysForRoom,
  selfRaisedHandFor,
} from "@/lib/calls/call-raised-hand";
import { expectedRemoteIdentitiesForCallState } from "@/lib/calls/call-tiles";
import {
  projectCallTiles,
  reconcileCallTileProjectionState,
} from "@/lib/calls/call-tile-projection";
import {
  $callPictureInPictureActive,
  $callPictureInPictureMode,
  $callPictureInPictureSupport,
  enterDocumentCallPictureInPicture,
  enterVideoCallPictureInPicture,
  exitVideoCallPictureInPicture,
  findCallPictureInPictureVideo,
  installVideoPictureInPictureCloseHandlers,
  refreshCallPictureInPictureSupport,
  selectCallPictureInPictureTile,
} from "@/lib/calls/call-picture-in-picture";
import type { BrowserXmppClient } from "@/lib/xmpp/client";
import CallTileGrid from "./CallTileGrid.vue";
import CallStageHeader from "./CallStageHeader.vue";
import CallControls from "./CallControls.vue";
import CallSettingsDialog from "./CallSettingsDialog.vue";
import CallMediaNotice from "./CallMediaNotice.vue";
import CallSelfShareNotice from "./CallSelfShareNotice.vue";
import CallPictureInPicturePanel from "./CallPictureInPicturePanel.vue";
import SplitDragHandle from "./SplitDragHandle.vue";

/**
 * Inline split-view surface for an active call in the current
 * conversation.
 *
 * Mounted by `ContentArea.vue` between the channel header/banners
 * and the message timeline. Renders only when ALL of the following
 * MUC calls also require the local user's nick to be in that room's
 * Muji-active list; 1:1 calls are keyed by the peer's bare JID.
 *
 * The split lives ABOVE the timeline so the chat is always visible
 * beneath; the drag handle resizes the two regions inside the parent
 * flex column. See `use-split-resize.ts` for the clamp + persistence
 * contract.
 */
const props = defineProps<{
  roomJid?: string;
  dmPeerJid?: string;
  dmPeerName?: string;
  xmppClient?: BrowserXmppClient | null;
  spaceId?: string | null;
  channelId?: string | null;
}>();

const state = useStore($callState);
const uiMode = useStore($callUiMode);
const viewMode = useStore($callViewMode);
const pinnedTileKey = useStore($callPinnedTileKey);
const selfViewHidden = useStore($callSelfViewHidden);
const lastError = useStore($lastCallError);
const connecting = useStore($callConnecting);
const micEnabled = useStore($callMicEnabled);
const camEnabled = useStore($callCamEnabled);
const screenShareEnabled = useStore($callScreenShareEnabled);
const screenShareSupported = useStore($callScreenShareSupported);
const pictureInPictureSupport = useStore($callPictureInPictureSupport);
const pictureInPictureActive = useStore($callPictureInPictureActive);
const pictureInPictureMode = useStore($callPictureInPictureMode);
const { remoteTracks, localTracks, engine, activeSpeakerIdentities, promotedSpeakerIdentity } =
  useCallEngine();
const { activeRoomJid, selfInCall } = useActiveMucCall();

const settingsOpen = ref(false);
const containerRef = ref<HTMLElement | null>(null);
const reactions = useStore($inCallReactions);
let reactionExpiryTimer: ReturnType<typeof setInterval> | null = null;

const activeCallReactions = computed(() => {
  return activeInCallReactions(state.value, reactions.value);
});
const pictureInPicturePanelRef = ref<HTMLElement | null>(null);
const pictureInPictureParkingRef = ref<HTMLElement | null>(null);
const mountedPictureInPictureVideo = ref<HTMLVideoElement | null>(null);
const pictureInPictureSeenRemoteScreenTrackKeys = ref<ReadonlySet<string>>(new Set());
let documentPictureInPictureWindow: Window | null = null;
let videoPictureInPictureElement: HTMLVideoElement | null = null;
let cleanupVideoPictureInPictureListeners: (() => void) | null = null;
let observedPictureInPictureRoot: HTMLElement | null = null;
let pictureInPictureVideoObserver: MutationObserver | null = null;

const normalizedRoomJid = computed(() =>
  normalizeMucCallRoomJid(props.roomJid ?? ""),
);

// The split surface has no room for the dock itself; it only needs the
// attendee count for the Participants button and the unread count for the
// Chat button, both of which bump to Expanded (where the dock reflows the
// stage) on click.
const { rows: rosterRows } = useCallRoster(normalizedRoomJid);
const participantCount = computed(() => rosterRows.value.length);
const chatUnread = useStore($callChatUnread);

function enterExpandedWithDock(): void {
  setCallDockTab("participants");
  openCallDock();
  $callUiMode.set("expanded");
}

function enterExpandedWithChat(): void {
  setCallDockTab("chat");
  openCallDock();
  $callUiMode.set("expanded");
}

const normalizedDmPeerJid = computed(() =>
  barePeerJid(props.dmPeerJid ?? "").toLowerCase(),
);

const ownsMucCall = computed(() => {
  return (
    state.value.phase === "active" &&
    state.value.kind === "muc" &&
    activeRoomJid.value !== null &&
    activeRoomJid.value === normalizedRoomJid.value
  );
});

const ownsDmCall = computed(() => {
  if (state.value.phase !== "active" || state.value.kind !== "dm") return false;
  const peer = barePeerJid(state.value.peer).toLowerCase();
  return !!peer && peer === normalizedDmPeerJid.value;
});

const shouldRender = computed(() => {
  return (
    state.value.phase === "active" &&
    uiMode.value === "split" &&
    ((state.value.kind === "muc" && ownsMucCall.value && selfInCall.value) ||
      (state.value.kind === "dm" && ownsDmCall.value))
  );
});

const splitKey = computed(() =>
  normalizedRoomJid.value || normalizedDmPeerJid.value,
);
const { isDragging, percent, beginDrag } = useSplitResize(splitKey);

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
  if (state.value.kind === "dm") {
    return state.value.media.video ? "Video call" : "Audio call";
  }
  const others = remoteParticipantCount.value;
  return others === 0
    ? "Waiting for others to join…"
    : `${others} other${others === 1 ? "" : "s"} in call`;
});

const callLabel = computed(() => {
  if (state.value.phase === "active" && state.value.kind === "dm") {
    return props.dmPeerName || peerLabelFromState();
  }
  return peerLabelFromState();
});

const localIdentity = computed(() => engine.localIdentity);

// #1029: raised-hand identity keys for the split-view tiles, self folded
// in optimistically (mirrors CallExpandedSurface).
const raisedHands = useStore($mucRaisedHands);
const selfRaisedHandStore = useStore($selfRaisedHand);
const raisedHandIdentityKeys = computed(() => {
  const keys = new Set(
    raisedHandKeysForRoom(normalizedRoomJid.value, raisedHands.value),
  );
  const selfKey = fullJidIdentityKey(localIdentity.value);
  if (selfRaisedHandFor(normalizedRoomJid.value, selfRaisedHandStore.value) && selfKey) {
    keys.add(selfKey);
  }
  return keys;
});
const selfSharePresentation = computed(() =>
  localScreenSharePresentation({
    screenShareEnabled: screenShareEnabled.value,
    localTracks: localTracks.value,
  }),
);
const stageLocalTracks = computed(() =>
  selfSharePresentation.value?.stageLocalTracks ?? localTracks.value,
);
const expectedRemoteIdentities = computed(() =>
  expectedRemoteIdentitiesForCallState(state.value),
);
const pictureInPictureProjection = computed(() =>
  projectCallTiles({
    remoteTracks: remoteTracks.value,
    localTracks: stageLocalTracks.value,
    localIdentity: localIdentity.value,
    expectedRemoteIdentities: expectedRemoteIdentities.value,
    micEnabled: micEnabled.value,
    seenRemoteScreenTrackKeys: pictureInPictureSeenRemoteScreenTrackKeys.value,
    pinnedTileKey: pinnedTileKey.value,
    viewMode: viewMode.value,
    promotedSpeakerIdentity: promotedSpeakerIdentity.value,
    hideSelfView: selfViewHidden.value,
  }),
);
watch(pictureInPictureProjection, (next) => {
  const reconciled = reconcileCallTileProjectionState({
    tiles: next.tiles,
    pinnedTileKey: pinnedTileKey.value,
    currentSeenRemoteScreenTrackKeys: pictureInPictureSeenRemoteScreenTrackKeys.value,
    nextSeenRemoteScreenTrackKeys: next.seenRemoteScreenTrackKeys,
  });
  if (
    pictureInPictureSeenRemoteScreenTrackKeys.value !==
    reconciled.seenRemoteScreenTrackKeys
  ) {
    pictureInPictureSeenRemoteScreenTrackKeys.value = reconciled.seenRemoteScreenTrackKeys;
  }
}, { immediate: true });
const pictureInPictureTile = computed(() => {
  return selectCallPictureInPictureTile({
    tiles: pictureInPictureProjection.value.tiles,
    pinnedTileKey: pinnedTileKey.value,
    activeSpeakerIdentities: activeSpeakerIdentities.value,
  });
});
const pictureInPictureSupported = computed(() => {
  if (!pictureInPictureTile.value) return false;
  if (pictureInPictureSupport.value === "document") return true;
  if (pictureInPictureSupport.value === "video") {
    return mountedPictureInPictureVideo.value !== null;
  }
  return false;
});

function refreshMountedPictureInPictureVideo(): void {
  if (pictureInPictureSupport.value !== "video") {
    mountedPictureInPictureVideo.value = null;
    return;
  }
  const tile = pictureInPictureTile.value;
  mountedPictureInPictureVideo.value = tile
    ? findCallPictureInPictureVideo(containerRef.value, tile.key)
    : null;
}

function observeMountedPictureInPictureVideos(root: HTMLElement | null): void {
  if (root === observedPictureInPictureRoot) return;
  disconnectMountedPictureInPictureVideoObserver();
  observedPictureInPictureRoot = root;
  if (!root || typeof MutationObserver === "undefined") return;
  pictureInPictureVideoObserver = new MutationObserver(() => {
    refreshMountedPictureInPictureVideo();
  });
  pictureInPictureVideoObserver.observe(root, { childList: true, subtree: true });
}

function disconnectMountedPictureInPictureVideoObserver(): void {
  pictureInPictureVideoObserver?.disconnect();
  pictureInPictureVideoObserver = null;
  observedPictureInPictureRoot = null;
}

watch([pictureInPictureTile, pictureInPictureSupport, shouldRender], async () => {
  await nextTick();
  observeMountedPictureInPictureVideos(shouldRender.value ? containerRef.value : null);
  refreshMountedPictureInPictureVideo();
}, { immediate: true, flush: "post" });

watch(shouldRender, (rendering) => {
  if (!rendering && hasLocalPictureInPictureHandle()) {
    void closePictureInPictureSafely();
  }
});

onMounted(() => {
  refreshScreenShareSupported();
  reactionExpiryTimer = setInterval(() => expireInCallReactions(), 400);
  refreshCallPictureInPictureSupport();
});

onBeforeUnmount(() => {
  if (reactionExpiryTimer) {
    clearInterval(reactionExpiryTimer);
    reactionExpiryTimer = null;
  }
  clearInCallReactions();
  disconnectMountedPictureInPictureVideoObserver();
  try {
    documentPictureInPictureWindow?.close();
  } finally {
    resetLocalPictureInPictureState();
  }
});

async function onHangup(): Promise<void> {
  await closePictureInPictureSafely();
  await hangupActiveCall();
}

async function onSendInCallReaction(emoji: string): Promise<void> {
  const active = state.value;
  if (active.phase !== "active" || !props.xmppClient) return;
  if (active.kind === "muc") {
    if (!props.spaceId || !props.channelId) return;
    await props.xmppClient.sendInCallReaction(props.spaceId, props.channelId, active.sid, emoji);
  } else {
    await props.xmppClient.sendDmInCallReaction(active.peer, active.sid, emoji);
    receiveInCallReaction({
      sid: active.sid,
      emoji,
      from: active.selfFullJid ?? "self",
    });
  }
}

// #1029: raise-hand toggle for the split-view control bar (MUC calls only).
const isMucCall = computed(
  () => state.value.phase === "active" && state.value.kind === "muc",
);
const selfHandRaised = computed(() =>
  selfRaisedHandFor(normalizedRoomJid.value, selfRaisedHandStore.value),
);
async function onToggleRaisedHand(): Promise<void> {
  if (!props.xmppClient) return;
  try {
    await props.xmppClient.setCallHandRaised(!selfHandRaised.value);
  } catch (err) {
    reportCallError(err);
  }
}

function parkPictureInPicturePanel(): void {
  const panel = pictureInPicturePanelRef.value;
  const parking = pictureInPictureParkingRef.value;
  if (!panel || !parking || panel.parentElement === parking) return;
  parking.appendChild(panel);
}

function onDocumentPictureInPictureClose(): void {
  resetLocalPictureInPictureState();
}

function onVideoPictureInPictureClose(): void {
  resetLocalPictureInPictureState();
}

function hasLocalPictureInPictureHandle(): boolean {
  return documentPictureInPictureWindow !== null || videoPictureInPictureElement !== null;
}

function resetLocalPictureInPictureState(): void {
  cleanupVideoPictureInPictureListeners?.();
  cleanupVideoPictureInPictureListeners = null;
  documentPictureInPictureWindow = null;
  videoPictureInPictureElement = null;
  $callPictureInPictureActive.set(false);
  $callPictureInPictureMode.set(null);
  parkPictureInPicturePanel();
}

async function closePictureInPicture(): Promise<void> {
  if (documentPictureInPictureWindow) {
    const pipWindow = documentPictureInPictureWindow;
    documentPictureInPictureWindow = null;
    try {
      pipWindow.close();
    } finally {
      resetLocalPictureInPictureState();
    }
    return;
  }
  await exitVideoCallPictureInPicture(videoPictureInPictureElement);
  resetLocalPictureInPictureState();
}

async function closePictureInPictureSafely(): Promise<void> {
  try {
    await closePictureInPicture();
  } catch {
    resetLocalPictureInPictureState();
  }
}

async function togglePictureInPicture(): Promise<void> {
  if (pictureInPictureActive.value) {
    await closePictureInPictureSafely();
    return;
  }
  const tile = pictureInPictureTile.value;
  if (!tile) return;
  if (pictureInPictureSupport.value === "document") {
    const panel = pictureInPicturePanelRef.value;
    if (!panel) return;
    const pipWindow = await enterDocumentCallPictureInPicture(panel, { width: 360, height: 240 });
    documentPictureInPictureWindow = pipWindow;
    pipWindow.addEventListener("pagehide", onDocumentPictureInPictureClose, { once: true });
    return;
  }
  const video = mountedPictureInPictureVideo.value
    ?? findCallPictureInPictureVideo(containerRef.value, tile.key);
  if (!video) return;
  await enterVideoCallPictureInPicture(video);
  videoPictureInPictureElement = video;
  cleanupVideoPictureInPictureListeners =
    installVideoPictureInPictureCloseHandlers(video, onVideoPictureInPictureClose);
  $callPictureInPictureActive.set(true);
  $callPictureInPictureMode.set("video");
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
      <CallStageHeader :title="callLabel" :subline="subline" compact />
      <div
        v-if="lastError"
        class="call-split__error"
        role="alert"
      >
        {{ lastError }}
      </div>
      <CallMediaNotice />
      <CallSelfShareNotice
        v-if="selfSharePresentation"
        :presentation="selfSharePresentation"
        compact
      />
      <div class="call-split__grid">
        <div
          v-if="pictureInPictureMode === 'document'"
          class="call-split__pip-placeholder"
        >
          <button
            type="button"
            class="chat-action-button chat-action-button--secondary"
            @click="togglePictureInPicture"
          >
            Return to call
          </button>
        </div>
        <CallTileGrid
          v-else
          :remote-tracks="remoteTracks"
          :local-tracks="stageLocalTracks"
          :local-identity="localIdentity"
          :expected-remote-identities="expectedRemoteIdentities"
          :mic-enabled="micEnabled"
          :active-speaker-identities="activeSpeakerIdentities"
          :promoted-speaker-identity="promotedSpeakerIdentity"
          :raised-hand-keys="raisedHandIdentityKeys"
          @open-participants="enterExpandedWithDock"
        />
        <div class="call-split__reactions" aria-live="polite" aria-atomic="false">
          <span
            v-for="reaction in activeCallReactions"
            :key="reaction.id"
            class="call-split__reaction"
            :style="{ '--reaction-x': `${Math.abs(hashInCallReactionId(reaction.id)) % 72}%` }"
          >
            {{ reaction.emoji }}
          </span>
        </div>
      </div>
      <footer class="call-split__footer">
        <CallControls
          :mic-enabled="micEnabled"
          :cam-enabled="camEnabled"
          :screen-share-enabled="screenShareEnabled"
          :screen-share-supported="screenShareSupported"
          :is-expanded="false"
          :participants-open="false"
          :participant-count="participantCount"
          :chat-open="false"
          :chat-unread="chatUnread"
          :view-mode="viewMode"
          :self-view-hidden="selfViewHidden"
          :picture-in-picture-supported="pictureInPictureSupported"
          :picture-in-picture-active="pictureInPictureActive"
          :raised-hand-available="isMucCall"
          :raised-hand-active="selfHandRaised"
          @toggle-mic="toggleMic"
          @toggle-cam="toggleCam"
          @toggle-screen-share="toggleScreenShare"
          @toggle-expanded="enterExpanded"
          @toggle-participants="enterExpandedWithDock"
          @toggle-chat="enterExpandedWithChat"
          @open-settings="settingsOpen = true"
          @set-view-mode="setCallViewMode"
          @toggle-self-view="toggleCallSelfViewHidden"
          @send-reaction="onSendInCallReaction"
          @toggle-raised-hand="onToggleRaisedHand"
          @toggle-picture-in-picture="togglePictureInPicture"
          @hangup="onHangup"
        />
      </footer>
      <div ref="pictureInPictureParkingRef" class="call-split__pip-parking" aria-hidden="true">
        <div ref="pictureInPicturePanelRef">
          <CallPictureInPicturePanel
            :tile="pictureInPictureTile"
            :mic-enabled="micEnabled"
            :cam-enabled="camEnabled"
            @toggle-mic="toggleMic"
            @toggle-cam="toggleCam"
            @return-to-call="togglePictureInPicture"
            @hangup="onHangup"
          />
        </div>
      </div>
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

.call-split__error {
  padding: 0.5rem 0.75rem;
  border-bottom: 1px solid color-mix(in oklab, var(--destructive) 30%, transparent);
  background: color-mix(in oklab, var(--destructive) 10%, transparent);
  color: var(--destructive);
}

.call-split__grid {
  position: relative;
  flex: 1 1 0%;
  min-height: 0;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.call-split__reactions {
  pointer-events: none;
  position: absolute;
  inset: 0;
  z-index: 2;
  overflow: hidden;
}

.call-split__reaction {
  position: absolute;
  left: calc(14% + var(--reaction-x, 0%));
  bottom: 16%;
  font-size: 2rem;
  line-height: 1;
  filter: drop-shadow(0 0.25rem 0.5rem rgb(0 0 0 / 0.28));
  animation: call-split-reaction-float 2.4s ease-out forwards;
}

@keyframes call-split-reaction-float {
  0% {
    opacity: 0;
    transform: translate3d(0, 0.75rem, 0) scale(0.82);
  }
  12% {
    opacity: 1;
    transform: translate3d(0, 0, 0) scale(1);
  }
  100% {
    opacity: 0;
    transform: translate3d(0, -8rem, 0) scale(1.18);
  }
}

.call-split__pip-placeholder {
  display: grid;
  place-items: center;
  width: 100%;
  height: 100%;
  min-height: 8rem;
  background: var(--muted);
  color: var(--muted-foreground);
}

.call-split__footer {
  padding: 0.5rem 0.75rem;
  border-top: 1px solid var(--border);
}

.call-split__pip-parking {
  display: none;
}
</style>
