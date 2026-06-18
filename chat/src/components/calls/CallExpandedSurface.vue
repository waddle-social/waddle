<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { useStore } from "@nanostores/vue";
import {
  $callState,
  $lastCallError,
} from "@/lib/calls/call-store";
import { $callUiMode } from "@/lib/calls/ui-mode";
import { $callViewMode, setCallViewMode } from "@/lib/calls/call-view-state";
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
import { barePeerJid } from "@/lib/xmpp/jid";
import { expectedRemoteIdentitiesForCallState } from "@/lib/calls/call-tiles";
import type { MentionCandidate } from "@/lib/mentions";
import type { MarkupSpan, MessageReference, TimelineMessage } from "@/lib/chat-ui";
import type { ComposerLinkPreviewLookup, ComposerLinkPreviewSendPayload } from "@/lib/link-preview-composer";
import CallTileGrid from "./CallTileGrid.vue";
import CallStageHeader from "./CallStageHeader.vue";
import CallControls from "./CallControls.vue";
import CallSettingsDialog from "./CallSettingsDialog.vue";
import CallMediaNotice from "./CallMediaNotice.vue";
import CallSelfShareNotice from "./CallSelfShareNotice.vue";
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
  giphyApiKey?: string;
  mentionCandidates?: MentionCandidate[];
  slowModeCooldown?: number;
  uploadProgress?: { uploading: boolean; progress: number; filename: string };
  linkPreviewLookup?: ComposerLinkPreviewLookup | null;
  linkPreviewScope?: string | null;
  /** The active call's XEP-0201 thread messages, rendered in the Chat tab. */
  callChatMessages?: readonly TimelineMessage[];
  avatarUrlByAuthor?: Record<string, string | null>;
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

const state = useStore($callState);
const uiMode = useStore($callUiMode);
const viewMode = useStore($callViewMode);
const lastError = useStore($lastCallError);
const connecting = useStore($callConnecting);
const micEnabled = useStore($callMicEnabled);
const camEnabled = useStore($callCamEnabled);
const screenShareEnabled = useStore($callScreenShareEnabled);
const screenShareSupported = useStore($callScreenShareSupported);
const { remoteTracks, localTracks, engine, activeSpeakerIdentities, promotedSpeakerIdentity } =
  useCallEngine();
const { activeRoomJid, selfInCall } = useActiveMucCall();

const settingsOpen = ref(false);
const dockOpen = useStore($callDockOpen);
const dockTab = useStore($callDockTab);
const chatUnread = useStore($callChatUnread);
const callChatDraft = ref("");

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
    uiMode.value === "expanded" &&
    ((state.value.kind === "muc" && ownsMucCall.value && selfInCall.value) ||
      (state.value.kind === "dm" && ownsDmCall.value))
  );
});

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
  return state.value.media.video ? "Video call" : "Audio call";
});

const callThreadOverride = computed(() => {
  const threadId = props.callThreadId?.trim();
  return threadId ? { threadId } : null;
});

const isMucCall = computed(
  () => state.value.phase === "active" && state.value.kind === "muc",
);

const localIdentity = computed(() => engine.localIdentity);
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

const {
  rows: rosterRows,
  setVolume: setParticipantVolume,
  resetAll: resetParticipantVolumes,
} = useCallRoster(normalizedRoomJid);

const participantCount = computed(() => rosterRows.value.length);

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
  // The dock is non-modal but still a layer the user opened on top of the
  // stage; Escape closes it before it collapses the call.
  if (dockOpen.value) {
    event.preventDefault();
    closeCallDock();
    return;
  }
  event.preventDefault();
  collapseToSplit();
}

onMounted(() => {
  refreshScreenShareSupported();
  if (typeof window !== "undefined") {
    window.addEventListener("keydown", onKeydown, true);
  }
});

onBeforeUnmount(() => {
  if (typeof window !== "undefined") {
    window.removeEventListener("keydown", onKeydown, true);
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
    <CallStageHeader :title="peerLabel" :subline="subline" />
    <div
      v-if="lastError"
      class="call-expanded__error"
      role="alert"
    >
      {{ lastError }}
    </div>
    <CallMediaNotice />
    <CallSelfShareNotice
      v-if="selfSharePresentation"
      :presentation="selfSharePresentation"
    />
    <main class="call-expanded__main">
      <div class="call-expanded__grid">
        <CallTileGrid
          :remote-tracks="remoteTracks"
          :local-tracks="stageLocalTracks"
          :local-identity="localIdentity"
          :expected-remote-identities="expectedRemoteIdentities"
          :mic-enabled="micEnabled"
          :active-speaker-identities="activeSpeakerIdentities"
          :promoted-speaker-identity="promotedSpeakerIdentity"
          @open-participants="openCallParticipants"
        />
      </div>
      <CallDock
        v-if="dockOpen"
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
            :avatar-url-by-author="avatarUrlByAuthor ?? {}"
            :is-sending="!!isSending"
            :disabled="!!disabled || !callThreadOverride"
            :giphy-api-key="giphyApiKey ?? ''"
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
    </main>
    <footer class="call-expanded__footer">
      <CallControls
        :mic-enabled="micEnabled"
        :cam-enabled="camEnabled"
        :screen-share-enabled="screenShareEnabled"
        :screen-share-supported="screenShareSupported"
        :is-expanded="true"
        :participants-open="participantsOpen"
        :participant-count="participantCount"
        :chat-open="chatOpen"
        :chat-unread="chatUnread"
        :view-mode="viewMode"
        @toggle-mic="toggleMic"
        @toggle-cam="toggleCam"
        @toggle-screen-share="toggleScreenShare"
        @toggle-expanded="collapseToSplit"
        @toggle-participants="toggleCallParticipants"
        @toggle-chat="toggleCallChat"
        @open-settings="settingsOpen = true"
        @set-view-mode="setCallViewMode"
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

.call-expanded__error {
  border-bottom: 1px solid color-mix(in oklab, var(--destructive) 30%, transparent);
  background: color-mix(in oklab, var(--destructive) 10%, transparent);
  padding: 0.5rem 1rem;
  color: var(--destructive);
}

.call-expanded__main {
  flex: 1 1 0%;
  min-height: 0;
  display: flex;
  overflow: hidden;
}

.call-expanded__grid {
  flex: 1 1 0%;
  min-width: 0;
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

/* On narrow viewports the dock becomes a full-width bottom panel (see
 * CallDock's own ≤760px rule). Match that here by stacking the stage above
 * it, so the grid stays visible instead of being squeezed beside a
 * 100%-wide dock in a row layout. */
@media (max-width: 760px) {
  .call-expanded__main {
    flex-direction: column;
  }
}
</style>
