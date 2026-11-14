<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";
import { useStore } from "@nanostores/vue";
import { PictureInPicture2 } from "lucide-vue-next";
import { css } from "styled-system/css";
import { btn } from "styled-system/recipes";
import {
  $callState,
  $lastCallError,
} from "@/lib/calls/call-store";
import {
  $callUiMode,
  callUiModeAfterFullscreenExit,
  callUiModeAfterSurfaceEscape,
  shouldExitNativeFullscreenForModeChange,
} from "@/lib/calls/ui-mode";
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
  setPushToTalkActive,
  toggleCam,
  toggleMic,
  toggleScreenShare,
} from "@/lib/calls/call-controls";
import { useCallShortcuts } from "@/lib/calls/use-call-shortcuts";
import { useCallEngine } from "@/lib/calls/use-call-engine";
import { useActiveMucCall } from "@/lib/calls/use-active-muc-call";
import { localScreenSharePresentation } from "@/lib/calls/call-self-share";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { useCallRoster } from "@/lib/calls/use-call-roster";
import {
  $callDockOpen,
  $callDockTab,
  closeCallDock,
  openCallParticipants,
  setCallDockTab,
  toggleCallChat,
  toggleCallParticipants,
} from "@/lib/calls/call-dock-state";
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
import {
  $mucMutedParticipants,
  mutedKeysForRoom,
} from "@/lib/calls/call-mute";
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
import type { MentionCandidate } from "@/lib/mentions";
import type { MarkupSpan, MessageReference, TimelineMessage } from "@/lib/chat-ui";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import type { BrowserXmppClient } from "@/lib/xmpp/client";
import CallTileGrid from "./CallTileGrid.vue";
import CallStageHeader from "./CallStageHeader.vue";
import CallControls from "./CallControls.vue";
import CallSettingsDialog from "./CallSettingsDialog.vue";
import CallMediaNotice from "./CallMediaNotice.vue";
import CallSelfShareNotice from "./CallSelfShareNotice.vue";
import CallPictureInPicturePanel from "./CallPictureInPicturePanel.vue";
import CallDock from "./CallDock.vue";
import CallChatPanel from "./CallChatPanel.vue";

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
 * MUC calls match the current room and require the local user to be
 * present in Muji. 1:1 calls match the current DM peer's bare JID.
 */
const props = defineProps<{
  roomJid?: string;
  dmPeerJid?: string;
  dmPeerName?: string;
  callThreadId?: string | null;
  isSending?: boolean;
  disabled?: boolean;
  mentionCandidates?: MentionCandidate[];
  slowModeCooldown?: number;
  uploadProgress?: { uploading: boolean; progress: number; filename: string };
  linkPreviewLookup?: ComposerLinkPreviewLookup | null;
  linkPreviewScope?: string | null;
  /** The active call's XEP-0201 thread messages, rendered in the Chat tab. */
  callChatMessages?: readonly TimelineMessage[];
  /** Real bare JID behind a call-chat author, for the avatar; `null` = initials. */
  avatarJidFor?: (message: TimelineMessage) => string | null;
  xmppClient?: BrowserXmppClient | null;
  spaceId?: string | null;
  channelId?: string | null;
  sendCallChatMessage?: (
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: undefined,
    threadOverride: { threadId: string },
    linkPreview?: ComposerLinkPreviewSendPayload,
  ) => Promise<void> | void;
}>();

const emit = defineEmits<{
  sendCallChat: [
    body: string,
    markup: MarkupSpan[],
    references: MessageReference[],
    files: Array<File | Blob> | undefined,
    replyTo: undefined,
    threadOverride: { threadId: string },
    linkPreview?: ComposerLinkPreviewSendPayload,
  ];
}>();

const surfaceRef = ref<HTMLElement | null>(null);
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
const chromeVisible = ref(true);
const nativeFullscreenActive = ref(false);
const pictureInPicturePanelRef = ref<HTMLElement | null>(null);
const pictureInPictureParkingRef = ref<HTMLElement | null>(null);
const mountedPictureInPictureVideo = ref<HTMLVideoElement | null>(null);
const pictureInPictureSeenRemoteScreenTrackKeys = ref<ReadonlySet<string>>(new Set());
let chromeIdleTimer: number | null = null;
let documentPictureInPictureWindow: Window | null = null;
let videoPictureInPictureElement: HTMLVideoElement | null = null;
let cleanupVideoPictureInPictureListeners: (() => void) | null = null;
let observedPictureInPictureRoot: HTMLElement | null = null;
let pictureInPictureVideoObserver: MutationObserver | null = null;
const dockOpen = useStore($callDockOpen);
const dockTab = useStore($callDockTab);
const chatUnread = useStore($callChatUnread);
const reactions = useStore($inCallReactions);
const callChatDraft = ref("");
let reactionExpiryTimer: ReturnType<typeof setInterval> | null = null;

const activeCallReactions = computed(() => {
  return activeInCallReactions(state.value, reactions.value);
});

// The dock is shared with the Participants tab; split these out so the control
// bar buttons reflect which tab (if any) the open dock is parked on.
const chatOpen = computed(() => dockOpen.value && dockTab.value === "chat");
const participantsOpen = computed(() => dockOpen.value && dockTab.value === "participants");

const normalizedRoomJid = computed(() =>
  normalizeMucCallRoomJid(props.roomJid ?? ""),
);

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

const isOpen = computed(() => {
  return (
    state.value.phase === "active" &&
    (uiMode.value === "expanded" || uiMode.value === "immersive") &&
    ((state.value.kind === "muc" && ownsMucCall.value && selfInCall.value) ||
      (state.value.kind === "dm" && ownsDmCall.value))
  );
});

const isImmersive = computed(() => uiMode.value === "immersive");

const remoteParticipantCount = computed(() => {
  const ids = new Set<string>();
  for (const t of remoteTracks.value) ids.add(t.participantIdentity);
  return ids.size;
});

const peerLabel = computed(() => {
  if (state.value.phase === "active" && state.value.kind === "dm") {
    return props.dmPeerName || peerLabelFromState();
  }
  return peerLabelFromState();
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
  return state.value.media.video ? "Video call" : "Voice call";
});

const callThreadOverride = computed(() => {
  const threadId = props.callThreadId?.trim();
  return threadId ? { threadId } : null;
});

const isMucCall = computed(
  () => state.value.phase === "active" && state.value.kind === "muc",
);

const localIdentity = computed(() => engine.localIdentity);

// #1029: raised-hand call presence state. The store is the inbound view
// (other occupants); self is folded in optimistically so the local tile
// and control-bar button reflect the toggle before the server echo.
const raisedHands = useStore($mucRaisedHands);
const selfRaisedHandStore = useStore($selfRaisedHand);
const selfHandRaised = computed(() =>
  selfRaisedHandFor(normalizedRoomJid.value, selfRaisedHandStore.value),
);
const raisedHandIdentityKeys = computed(() => {
  const keys = new Set(
    raisedHandKeysForRoom(normalizedRoomJid.value, raisedHands.value),
  );
  const selfKey = fullJidIdentityKey(localIdentity.value);
  if (selfHandRaised.value && selfKey) keys.add(selfKey);
  return keys;
});

// #1030: muted call presence state. Pure INBOUND-REMOTE view — self-mute
// is sourced from the local mic flag (`micEnabled`), so unlike raised-hand
// the local identity is NOT folded in here.
const mutedParticipants = useStore($mucMutedParticipants);
const mutedIdentityKeys = computed(() =>
  mutedKeysForRoom(normalizedRoomJid.value, mutedParticipants.value),
);

async function onToggleRaisedHand(): Promise<void> {
  if (!props.xmppClient) return;
  try {
    await props.xmppClient.setCallHandRaised(!selfHandRaised.value);
  } catch (err) {
    reportCallError(err);
  }
}
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
    mutedKeys: mutedIdentityKeys.value,
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
    ? findCallPictureInPictureVideo(surfaceRef.value, tile.key)
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

watch([pictureInPictureTile, pictureInPictureSupport, isOpen], async () => {
  await nextTick();
  observeMountedPictureInPictureVideos(isOpen.value ? surfaceRef.value : null);
  refreshMountedPictureInPictureVideo();
}, { immediate: true, flush: "post" });

watch(isOpen, (open) => {
  if (!open && hasLocalPictureInPictureHandle()) {
    void closePictureInPictureSafely();
  }
});

const {
  rows: rosterRows,
  setVolume: setParticipantVolume,
  resetAll: resetParticipantVolumes,
} = useCallRoster(normalizedRoomJid);

const participantCount = computed(() => rosterRows.value.length);

async function toggleExpandedSurface(): Promise<void> {
  const nextMode = callUiModeAfterSurfaceEscape(uiMode.value);
  if (
    shouldExitNativeFullscreenForModeChange(
      uiMode.value,
      nextMode,
      nativeFullscreenActive.value,
    ) &&
    typeof document !== "undefined" &&
    document.fullscreenElement === surfaceRef.value
  ) {
    await document.exitFullscreen();
  }
  $callUiMode.set(nextMode);
}

function enterImmersive(): void {
  $callUiMode.set("immersive");
}

// #1034: keyboard shortcuts while the expanded/immersive surface is focused.
// Gated on `isOpen` because the split surface stays mounted alongside this one
// — without the gate every shortcut would fire twice. `F` enters immersive and
// requests native fullscreen (idempotent: it never toggles fullscreen back
// off). `Esc` is intentionally NOT mapped here: the bespoke `onKeydown` below
// already owns Escape (menu/settings/dock/PiP/collapse precedence).
useCallShortcuts(
  {
    "toggle-mic": () => void toggleMic(),
    "push-to-talk-start": () => setPushToTalkActive(true),
    "push-to-talk-end": () => setPushToTalkActive(false),
    "toggle-camera": () => void toggleCam(),
    "toggle-share": () => {
      // Mirror the hidden Share button: don't start capture where unsupported.
      if (!screenShareSupported.value) return false;
      void toggleScreenShare();
    },
    "toggle-raise-hand": () => void onToggleRaisedHand(),
    "enter-immersive": () => {
      enterImmersive();
      // requestFullscreen rejects if the gesture was consumed or denied;
      // swallow it so the immersive switch still stands.
      if (!nativeFullscreenActive.value) void toggleNativeFullscreen().catch(() => {});
    },
  },
  () => isOpen.value,
);

function clearChromeIdleTimer(): void {
  if (typeof window === "undefined") return;
  if (chromeIdleTimer === null) return;
  window.clearTimeout(chromeIdleTimer);
  chromeIdleTimer = null;
}

function chromeHasFocus(): boolean {
  if (typeof document === "undefined") return false;
  const active = document.activeElement;
  return active instanceof Element && !!active.closest(".call-expanded__chrome");
}

function scheduleImmersiveChromeHide(): void {
  if (typeof window === "undefined" || !isImmersive.value) return;
  chromeIdleTimer = window.setTimeout(() => {
    chromeIdleTimer = null;
    if (!isImmersive.value) return;
    if (chromeHasFocus()) {
      scheduleImmersiveChromeHide();
      return;
    }
    chromeVisible.value = false;
  }, 2_000);
}

function revealImmersiveChrome(): void {
  chromeVisible.value = true;
  if (typeof window === "undefined" || !isImmersive.value) return;
  clearChromeIdleTimer();
  scheduleImmersiveChromeHide();
}

async function toggleNativeFullscreen(): Promise<void> {
  if (typeof document === "undefined") return;
  if (document.fullscreenElement === surfaceRef.value) {
    await document.exitFullscreen();
    return;
  }
  if (document.fullscreenElement) return;
  await surfaceRef.value?.requestFullscreen?.();
}

function onFullscreenChange(): void {
  if (typeof document === "undefined") return;
  const wasNativeFullscreenActive = nativeFullscreenActive.value;
  nativeFullscreenActive.value = document.fullscreenElement === surfaceRef.value;
  if (wasNativeFullscreenActive && !nativeFullscreenActive.value) {
    $callUiMode.set(callUiModeAfterFullscreenExit(uiMode.value));
  }
}

async function onHangup(): Promise<void> {
  await closePictureInPictureSafely();
  await hangupActiveCall();
  // After hangup the surface unmounts on its own (phase flips out of
  // "active"); flip the mode back so the next call defaults to split
  // instead of inheriting "expanded".
  $callUiMode.set("split");
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
    ?? findCallPictureInPictureVideo(surfaceRef.value, tile.key);
  if (!video) return;
  await enterVideoCallPictureInPicture(video);
  videoPictureInPictureElement = video;
  cleanupVideoPictureInPictureListeners =
    installVideoPictureInPictureCloseHandlers(video, onVideoPictureInPictureClose);
  $callPictureInPictureActive.set(true);
  $callPictureInPictureMode.set("video");
}

async function onSendCallChat(
  body: string,
  markup: MarkupSpan[],
  references: MessageReference[],
  files?: Array<File | Blob>,
  linkPreview?: ComposerLinkPreviewSendPayload,
): Promise<void> {
  const override = callThreadOverride.value;
  if (!override) return;
  if (props.sendCallChatMessage) {
    await props.sendCallChatMessage(body, markup, references, files, undefined, override, linkPreview);
  } else {
    emit("sendCallChat", body, markup, references, files, undefined, override, linkPreview);
  }
  callChatDraft.value = "";
}

async function onSendInCallReaction(emoji: string): Promise<void> {
  const active = state.value;
  if (active.phase !== "active") return;
  if (active.kind === "muc") {
    if (!props.xmppClient || !props.spaceId || !props.channelId) return;
    await props.xmppClient.sendInCallReaction(props.spaceId, props.channelId, active.sid, emoji);
  } else {
    if (!props.xmppClient) return;
    await props.xmppClient.sendDmInCallReaction(active.peer, active.sid, emoji);
    receiveInCallReaction({
      sid: active.sid,
      emoji,
      from: active.selfFullJid ?? "self",
    });
  }
}

function onKeydown(event: KeyboardEvent): void {
  if (!isOpen.value) return;
  if (event.key !== "Escape") return;
  // The control bar's More overflow owns Escape while open. This listener runs
  // in the CAPTURE phase, so without this guard it would collapse the call
  // before the menu's own bubble-phase Escape handler could simply close the
  // menu. Defer to it: an open menu-button advertises `aria-expanded="true"`.
  if (
    typeof document !== "undefined" &&
    document.querySelector(
      '.call-controls [aria-haspopup="menu"][aria-expanded="true"]',
    )
  ) {
    return;
  }
  // The settings modal owns Escape while open: dismiss it rather than
  // collapsing the whole expanded call out from under it. This listener
  // runs in the CAPTURE phase so it sees the still-open flag before
  // AppDialog's own bubble-phase Escape handler flips the v-model to
  // false — otherwise a dialog dismiss from inside the dialog would fall
  // through to collapse.
  if (settingsOpen.value) {
    event.preventDefault();
    settingsOpen.value = false;
    return;
  }
  // Another modal dialog (e.g. the quick switcher) owns Escape while open:
  // let it close itself instead of collapsing the call underneath it.
  if (typeof document !== "undefined" && document.querySelector("[aria-modal='true']")) return;
  // The dock is non-modal but still a layer the user opened on top of the
  // stage; Escape closes it before it collapses the call.
  if (dockOpen.value) {
    event.preventDefault();
    closeCallDock();
    return;
  }
  // #1034: Picture-in-Picture is another layer above the stage — Escape pops
  // it before collapsing the surface, matching "Esc exits immersive/PiP".
  if (pictureInPictureActive.value) {
    event.preventDefault();
    void closePictureInPictureSafely();
    return;
  }
  event.preventDefault();
  void toggleExpandedSurface();
}

watch(isImmersive, (immersive) => {
  if (immersive) {
    revealImmersiveChrome();
  } else {
    clearChromeIdleTimer();
    chromeVisible.value = true;
  }
});

onMounted(() => {
  refreshScreenShareSupported();
  refreshCallPictureInPictureSupport();
  revealImmersiveChrome();
  if (typeof window !== "undefined") {
    window.addEventListener("keydown", onKeydown, true);
  }
  if (typeof document !== "undefined") {
    document.addEventListener("fullscreenchange", onFullscreenChange);
  }
  reactionExpiryTimer = setInterval(() => expireInCallReactions(), 400);
});

onBeforeUnmount(() => {
  clearChromeIdleTimer();
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
  if (typeof window !== "undefined") {
    window.removeEventListener("keydown", onKeydown, true);
  }
  if (typeof document !== "undefined") {
    document.removeEventListener("fullscreenchange", onFullscreenChange);
  }
});

// HuddleStage (expanded / immersive): the dark stage fills the chat pane
// (immersive: the window). `call-expanded` stays as the marker the call
// shortcuts scope to.
const rootClass = css({
  position: "absolute",
  inset: 0,
  zIndex: 30,
  display: "flex",
  flexDirection: "column",
  bgColor: "sea.deep",
  color: "fg",
  fontFamily: "body",
});
const immersiveClass = css({ position: "fixed", zIndex: 80 });
const chromeClass = css({
  transition: "opacity 180ms ease, transform 180ms ease, visibility 180ms ease",
  _motionReduce: { transition: "opacity 180ms ease, visibility 180ms ease" },
});
const chromeOverlayClass = css({ position: "absolute", insetInline: 0, zIndex: 3 });
const headerOverlayClass = css({ insetBlockStart: 0 });
const footerOverlayClass = css({ insetBlockEnd: 0 });
const chromeHiddenClass = css({ opacity: 0, pointerEvents: "none", visibility: "hidden" });
const headerHiddenClass = css({ transform: "translateY(-8px)" });
const footerHiddenClass = css({ transform: "translateY(8px)" });
const errorClass = css({
  margin: "0 16px 10px",
  padding: "10px 12px",
  borderRadius: "12px",
  bg: "color-mix(in srgb, {colors.danger} 14%, transparent)",
  border: "1px solid color-mix(in srgb, {colors.danger} 40%, transparent)",
  color: "danger.text",
  fontSize: "13px",
});
const mainClass = css({ flex: "1 1 0%", minHeight: 0, display: "flex", gap: "3", overflow: "hidden", "@media (max-width: 760px)": { flexDirection: "column" } });
const mainImmersiveClass = css({ position: "absolute", inset: 0, zIndex: 0 });
const gridClass = css({ flex: "1 1 0%", minWidth: 0, minHeight: 0, position: "relative", display: "flex", flexDirection: "column", overflow: "hidden" });
const reactionsClass = css({ pointerEvents: "none", position: "absolute", inset: 0, zIndex: 2, overflow: "hidden" });
const reactionClass = css({
  position: "absolute",
  left: "calc(14% + var(--reaction-x, 0%))",
  bottom: "16%",
  fontSize: "36px",
  lineHeight: 1,
  filter: "drop-shadow(0 4px 8px rgb(0 0 0 / 0.28))",
  animation: "call-reaction-float 2.4s ease-out forwards",
  _motionReduce: { animation: "wd-fade 2.4s ease-out reverse forwards" },
});
const pipClass = css({
  flex: 1,
  margin: "0 16px 12px",
  display: "flex",
  flexDirection: "column",
  alignItems: "center",
  justifyContent: "center",
  gap: "3",
  padding: "4",
  textAlign: "center",
  borderRadius: "12px",
  border: "1px dashed {colors.border}",
  "& > svg": { width: "40px", height: "40px", color: "fg.muted", strokeWidth: 1.5 },
  "& b": { font: "700 17px/1.3 {fonts.display}" },
  "& span": { color: "fg.muted", fontSize: "14px" },
});
const pipButtonClass = btn({ variant: "primary" });
const dockClass = css({ display: "contents" });
const dockImmersiveClass = css({
  position: "absolute",
  insetBlock: "72px 80px",
  insetInlineEnd: "16px",
  zIndex: 2,
  display: "flex",
  maxWidth: "min(384px, calc(100vw - 32px))",
  "@media (max-width: 760px)": { insetBlock: "auto 80px", insetInline: "12px", maxWidth: "none" },
});
const footerClass = css({ display: "flex", alignItems: "center", justifyContent: "center", padding: "12px 16px 16px", _narrow: { padding: "12px 12px 20px" } });
const parkingClass = css({ display: "none" });
</script>

<template>
  <div
    v-if="isOpen"
    ref="surfaceRef"
    :class="['call-expanded', rootClass, isImmersive && ['call-expanded--immersive', immersiveClass]]"
    data-theme="night"
    role="region"
    :aria-label="isImmersive ? 'Active call (immersive)' : 'Active call (expanded)'"
    tabindex="-1"
    @focusin="revealImmersiveChrome"
    @keydown="revealImmersiveChrome"
    @pointermove="revealImmersiveChrome"
  >
    <CallStageHeader
      :class="[
        'call-expanded__chrome',
        chromeClass,
        isImmersive && [chromeOverlayClass, headerOverlayClass],
        isImmersive && !chromeVisible && [chromeHiddenClass, headerHiddenClass],
      ]"
      :title="peerLabel"
      :subline="subline"
    />
    <div
      v-if="lastError"
      :class="errorClass"
      role="alert"
    >
      {{ lastError }}
    </div>
    <CallMediaNotice />
    <CallSelfShareNotice
      v-if="selfSharePresentation"
      :presentation="selfSharePresentation"
    />
    <main :class="[mainClass, isImmersive && mainImmersiveClass]">
      <div :class="gridClass">
        <div
          v-if="pictureInPictureMode === 'document'"
          :class="pipClass"
        >
          <PictureInPicture2 aria-hidden="true" />
          <b>The {{ isMucCall ? "huddle" : "call" }} is in Picture-in-Picture</b>
          <span>Keep chatting while it floats above your other windows.</span>
          <button
            type="button"
            :class="pipButtonClass"
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
          :muted-keys="mutedIdentityKeys"
          @open-participants="openCallParticipants"
        />
        <div :class="reactionsClass" aria-live="polite" aria-atomic="false">
          <span
            v-for="reaction in activeCallReactions"
            :key="reaction.id"
            :class="reactionClass"
            :style="{ '--reaction-x': `${Math.abs(hashInCallReactionId(reaction.id)) % 72}%` }"
          >
            {{ reaction.emoji }}
          </span>
        </div>
      </div>
      <div
        v-if="dockOpen"
        :class="['call-expanded__dock-overlay', isImmersive ? dockImmersiveClass : dockClass]"
      >
        <CallDock
          :rows="rosterRows"
          :active-tab="dockTab"
          :chat-unread="chatUnread"
          @set-tab="setCallDockTab"
          @set-volume="setParticipantVolume"
          @reset-all="resetParticipantVolumes"
          @close="closeCallDock"
        >
          <template #chat>
            <CallChatPanel
              v-model:draft="callChatDraft"
              :messages="callChatMessages ?? []"
              :visible="chatOpen"
              :avatar-jid-for="avatarJidFor ?? (() => null)"
              :is-sending="!!isSending"
              :disabled="!!disabled || !callThreadOverride"
              :mention-candidates="mentionCandidates ?? []"
              :slow-mode-cooldown="slowModeCooldown ?? 0"
              :upload-progress="uploadProgress ?? { uploading: false, progress: 0, filename: '' }"
              :in-muc="isMucCall"
              :link-preview-lookup="linkPreviewLookup ?? null"
              :link-preview-scope="linkPreviewScope ?? null"
              @send="onSendCallChat"
            />
          </template>
        </CallDock>
      </div>
    </main>
    <footer
      :class="[
        'call-expanded__chrome',
        chromeClass,
        footerClass,
        isImmersive && [chromeOverlayClass, footerOverlayClass],
        isImmersive && !chromeVisible && [chromeHiddenClass, footerHiddenClass],
      ]"
    >
      <CallControls
        :mic-enabled="micEnabled"
        :cam-enabled="camEnabled"
        :screen-share-enabled="screenShareEnabled"
        :screen-share-supported="screenShareSupported"
        :is-expanded="true"
        :is-immersive="isImmersive"
        :is-native-fullscreen="nativeFullscreenActive"
        :participants-open="participantsOpen"
        :participant-count="participantCount"
        :chat-open="chatOpen"
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
        @toggle-expanded="toggleExpandedSurface"
        @toggle-immersive="enterImmersive"
        @toggle-native-fullscreen="toggleNativeFullscreen"
        @toggle-participants="toggleCallParticipants"
        @toggle-chat="toggleCallChat"
        @open-settings="settingsOpen = true"
        @set-view-mode="setCallViewMode"
        @toggle-self-view="toggleCallSelfViewHidden"
        @send-reaction="onSendInCallReaction"
        @toggle-raised-hand="onToggleRaisedHand"
        @toggle-picture-in-picture="togglePictureInPicture"
        @hangup="onHangup"
      />
    </footer>
    <CallSettingsDialog v-model:open="settingsOpen" />
    <div ref="pictureInPictureParkingRef" :class="parkingClass" aria-hidden="true">
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
  </div>
</template>

<style>
/* Reaction float: Panda has no keyframe for it, so it lives here (unscoped,
   so the Panda class can name it). */
@keyframes call-reaction-float {
  0% { opacity: 0; transform: translate3d(0, 0.75rem, 0) scale(0.82); }
  12% { opacity: 1; transform: translate3d(0, 0, 0) scale(1); }
  100% { opacity: 0; transform: translate3d(0, -8rem, 0) scale(1.18); }
}
</style>
