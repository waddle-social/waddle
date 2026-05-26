<script setup lang="ts">
import { computed, ref } from "vue";
import { useStore } from "@nanostores/vue";
import {
  Maximize2,
  MessageCircle,
  Mic,
  MicOff,
  Phone,
  PhoneCall,
  PhoneOff,
  Settings,
  Users,
  Video,
  VideoOff,
} from "lucide-vue-next";
import { $callState } from "@/lib/calls/call-store";
import {
  $callCamEnabled,
  $callConnecting,
  $callMicEnabled,
  hangupActiveCall,
  toggleCam,
  toggleMic,
} from "@/lib/calls/call-controls";
import { $callUiMode } from "@/lib/calls/ui-mode";
import {
  $mucCallParticipants,
  normalizeMucCallRoomJid,
} from "@/lib/calls/muc-call-presence";
import { mucCallParticipantPreview } from "@/lib/calls/muc-call-indicators";
import { barePeerJid } from "@/lib/xmpp/jid";
import type { ChannelSummary } from "@/lib/chat-types";
import type { DmConversation } from "@/lib/xmpp-client";
import CallSettingsDialog from "./CallSettingsDialog.vue";

const props = defineProps<{
  channels: Pick<ChannelSummary, "id" | "name" | "jid">[];
  conversations: Pick<DmConversation, "peerJid" | "peerUsername">[];
  activeChannelId: string | null;
  activeChannelRoomJid?: string | null;
  activePeerJid: string | null;
}>();

const emit = defineEmits<{
  selectChannel: [channelId: string | null, roomJid: string];
  selectDm: [peerJid: string];
}>();

type CurrentCallPanelState = {
  kind: "dm" | "muc";
  title: string;
  targetJid: string;
  channelId: string | null;
  media: { audio: boolean; video: boolean };
  connected: boolean;
  viewing: boolean;
  participantCount: number;
  participantLabels: string[];
};

const state = useStore($callState);
const connecting = useStore($callConnecting);
const micEnabled = useStore($callMicEnabled);
const camEnabled = useStore($callCamEnabled);
const mucParticipants = useStore($mucCallParticipants);

const settingsOpen = ref(false);

const activeChannelRoom = computed(() =>
  normalizeMucCallRoomJid(props.activeChannelRoomJid ?? ""),
);

const panel = computed<CurrentCallPanelState | null>(() => {
  const current = state.value;
  if (current.phase !== "active" && current.phase !== "muc-pending") return null;

  if (current.kind === "muc") {
    const roomJid = normalizeMucCallRoomJid(current.peer);
    if (!roomJid) return null;
    const channel = props.channels.find((candidate) =>
      normalizeMucCallRoomJid(candidate.jid ?? "") === roomJid
    );
    const participantLabels = mucParticipants.value[roomJid] ?? [];
    const fallbackLabels = participantLabels.length > 0
      ? participantLabels
      : current.selfNick
        ? [current.selfNick]
        : [];
    const participantCount = Math.max(fallbackLabels.length, current.phase === "muc-pending" ? 1 : 0);
    return {
      kind: "muc",
      title: channel?.name ?? roomJid.split("@")[0] ?? "Group call",
      targetJid: roomJid,
      channelId: channel?.id ?? null,
      media: current.media,
      connected: current.phase === "active",
      viewing: (
        props.activeChannelId === channel?.id ||
        (!!activeChannelRoom.value && activeChannelRoom.value === roomJid)
      ),
      participantCount,
      participantLabels: fallbackLabels,
    };
  }

  const peerJid = barePeerJid(current.peer).toLowerCase();
  if (!peerJid) return null;
  const conversation = props.conversations.find((candidate) =>
    barePeerJid(candidate.peerJid).toLowerCase() === peerJid
  );
  return {
    kind: "dm",
    title: conversation?.peerUsername ?? peerJid.split("@")[0] ?? "Direct call",
    targetJid: peerJid,
    channelId: null,
    media: current.media,
    connected: true,
    viewing: barePeerJid(props.activePeerJid ?? "").toLowerCase() === peerJid,
    participantCount: 2,
    participantLabels: [],
  };
});

const isConnected = computed(() => panel.value?.connected === true && !connecting.value);
const callTypeLabel = computed(() => {
  const current = panel.value;
  if (!current) return "";
  if (current.kind === "muc") return "Group call";
  return current.media.video ? "Video call" : "Voice call";
});
const statusLabel = computed(() => {
  const current = panel.value;
  if (!current) return "";
  if (!current.connected) return "Starting call";
  if (connecting.value) return "Connecting...";
  return "Connected";
});
const participantLabel = computed(() => {
  const current = panel.value;
  if (!current) return "";
  if (current.kind === "dm") return "1:1";
  const preview = mucCallParticipantPreview(current.participantLabels);
  if (preview) return preview;
  const count = current.participantCount;
  if (count <= 0) return "Group";
  return `${count} in call`;
});
const returnLabel = computed(() => {
  const current = panel.value;
  if (!current) return "";
  return current.viewing ? "Viewing" : "Return";
});
const controlsDisabled = computed(() => panel.value?.connected !== true);

function returnToCall(): void {
  const current = panel.value;
  if (!current || current.viewing) return;
  $callUiMode.set("split");
  if (current.kind === "muc") {
    emit("selectChannel", current.channelId, current.targetJid);
    return;
  }
  emit("selectDm", current.targetJid);
}

function expandCall(): void {
  const current = panel.value;
  if (!current || !current.connected) return;
  $callUiMode.set("expanded");
  if (current.kind === "muc") {
    emit("selectChannel", current.channelId, current.targetJid);
    return;
  }
  emit("selectDm", current.targetJid);
}
</script>

<template>
  <aside
    v-if="panel"
    class="current-call-panel"
    aria-label="Current call"
  >
    <div class="current-call-panel__summary">
      <span class="current-call-panel__icon" :class="{ 'current-call-panel__icon--connecting': !isConnected }" aria-hidden="true">
        <Users v-if="panel.kind === 'muc'" class="h-3.5 w-3.5" />
        <Video v-else-if="panel.media.video" class="h-3.5 w-3.5" />
        <Phone v-else class="h-3.5 w-3.5" />
      </span>
      <span class="current-call-panel__copy">
        <span class="current-call-panel__title type-control">{{ panel.title }}</span>
        <span class="current-call-panel__status type-meta">
          <span class="current-call-panel__live-dot" :class="{ 'current-call-panel__live-dot--muted': !isConnected }" aria-hidden="true" />
          {{ statusLabel }} · {{ callTypeLabel }} · {{ participantLabel }}
        </span>
      </span>
      <button
        type="button"
        class="current-call-panel__return type-meta"
        :class="{ 'current-call-panel__return--active': panel.viewing }"
        :disabled="panel.viewing"
        :aria-label="`${returnLabel} ${panel.title} call`"
        @click="returnToCall"
      >
        <MessageCircle v-if="panel.kind === 'dm'" class="h-3 w-3" />
        <PhoneCall v-else class="h-3 w-3" />
        <span>{{ returnLabel }}</span>
      </button>
    </div>

    <div class="current-call-panel__controls" aria-label="Current call controls">
      <button
        type="button"
        class="current-call-panel__control"
        :class="{ 'current-call-panel__control--off': !micEnabled }"
        :disabled="controlsDisabled"
        :title="micEnabled ? 'Mute' : 'Unmute'"
        :aria-label="micEnabled ? 'Mute microphone' : 'Unmute microphone'"
        @click="toggleMic"
      >
        <Mic v-if="micEnabled" class="h-3.5 w-3.5" />
        <MicOff v-else class="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        class="current-call-panel__control"
        :class="{ 'current-call-panel__control--off': !camEnabled }"
        :disabled="controlsDisabled"
        :title="camEnabled ? 'Camera off' : 'Camera on'"
        :aria-label="camEnabled ? 'Turn camera off' : 'Turn camera on'"
        @click="toggleCam"
      >
        <Video v-if="camEnabled" class="h-3.5 w-3.5" />
        <VideoOff v-else class="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        class="current-call-panel__control"
        :disabled="controlsDisabled"
        title="Expand call"
        aria-label="Expand current call"
        @click="expandCall"
      >
        <Maximize2 class="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        class="current-call-panel__control"
        :disabled="controlsDisabled"
        title="Call settings"
        aria-label="Open call settings"
        @click="settingsOpen = true"
      >
        <Settings class="h-3.5 w-3.5" />
      </button>
      <button
        type="button"
        class="current-call-panel__control current-call-panel__control--danger"
        title="Hang up"
        aria-label="Hang up current call"
        @click="hangupActiveCall"
      >
        <PhoneOff class="h-3.5 w-3.5" />
      </button>
    </div>

    <CallSettingsDialog v-model:open="settingsOpen" />
  </aside>
</template>

<style scoped>
.current-call-panel {
  display: flex;
  flex-shrink: 0;
  flex-direction: column;
  gap: 0.5rem;
  border-top: 1px solid var(--border);
  border-right: 1px solid var(--border);
  background:
    linear-gradient(
      180deg,
      color-mix(in oklab, var(--sidebar-accent) 38%, var(--card)),
      color-mix(in oklab, var(--card) 94%, transparent)
    );
  padding: 0.625rem;
}

.current-call-panel__summary {
  display: grid;
  grid-template-columns: 1.875rem minmax(0, 1fr) auto;
  align-items: center;
  gap: 0.5rem;
  min-width: 0;
}

.current-call-panel__icon {
  display: inline-flex;
  height: 1.875rem;
  width: 1.875rem;
  align-items: center;
  justify-content: center;
  border-radius: 0.5rem;
  background: color-mix(in oklab, oklch(0.7 0.18 145) 18%, transparent);
  color: oklch(0.46 0.14 145);
}

.current-call-panel__icon--connecting {
  background: color-mix(in oklab, var(--primary) 14%, transparent);
  color: var(--primary);
}

.current-call-panel__copy {
  display: flex;
  min-width: 0;
  flex-direction: column;
  gap: 0.125rem;
}

.current-call-panel__title,
.current-call-panel__status {
  display: block;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.current-call-panel__title {
  color: var(--sidebar-foreground);
}

.current-call-panel__status {
  color: var(--sidebar-muted);
}

.current-call-panel__live-dot {
  display: inline-block;
  width: 0.4rem;
  height: 0.4rem;
  margin-right: 0.25rem;
  border-radius: 9999px;
  background: oklch(0.7 0.18 145);
  box-shadow: 0 0 0 3px color-mix(in oklab, oklch(0.7 0.18 145) 18%, transparent);
  vertical-align: 0.05em;
}

.current-call-panel__live-dot--muted {
  background: var(--primary);
  box-shadow: 0 0 0 3px color-mix(in oklab, var(--primary) 18%, transparent);
}

.current-call-panel__return,
.current-call-panel__control {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  border-radius: 0.5rem;
  transition:
    background-color 160ms ease,
    color 160ms ease,
    border-color 160ms ease,
    transform 160ms ease;
}

.current-call-panel__return {
  min-width: 4.25rem;
  height: 1.75rem;
  gap: 0.25rem;
  border: 1px solid color-mix(in oklab, var(--primary) 24%, transparent);
  background: color-mix(in oklab, var(--primary) 10%, transparent);
  color: var(--primary);
  padding: 0 0.5rem;
  white-space: nowrap;
}

.current-call-panel__return:hover {
  background: color-mix(in oklab, var(--primary) 16%, transparent);
}

.current-call-panel__return:disabled {
  cursor: default;
}

.current-call-panel__return--active {
  border-color: color-mix(in oklab, oklch(0.7 0.18 145) 34%, transparent);
  background: color-mix(in oklab, oklch(0.7 0.18 145) 16%, transparent);
  color: oklch(0.46 0.14 145);
}

.current-call-panel__controls {
  display: grid;
  grid-template-columns: repeat(5, minmax(0, 1fr));
  gap: 0.375rem;
}

.current-call-panel__control {
  height: 2rem;
  min-width: 0;
  border: 1px solid transparent;
  background: color-mix(in oklab, var(--sidebar-accent) 68%, transparent);
  color: var(--sidebar-foreground);
}

.current-call-panel__control:hover:not(:disabled) {
  transform: translateY(-1px);
  background: color-mix(in oklab, var(--sidebar-accent) 90%, transparent);
  color: var(--primary);
}

.current-call-panel__control:disabled {
  cursor: not-allowed;
  opacity: 0.48;
}

.current-call-panel__control--off {
  border-color: color-mix(in oklab, var(--primary) 26%, transparent);
  background: color-mix(in oklab, var(--primary) 12%, transparent);
  color: var(--primary);
}

.current-call-panel__control--danger {
  background: color-mix(in oklab, var(--destructive) 12%, transparent);
  color: var(--destructive);
}

.current-call-panel__control--danger:hover:not(:disabled) {
  background: color-mix(in oklab, var(--destructive) 18%, transparent);
  color: var(--destructive);
}

.current-call-panel.current-call-panel--mobile {
  border-top: 0;
  border-right: 0;
  border-bottom: 1px solid var(--border);
  padding: 0.5rem 0.75rem;
}

@media (min-width: 64rem) {
  .current-call-panel.current-call-panel--mobile {
    display: none;
  }
}

@media (prefers-reduced-motion: reduce) {
  .current-call-panel__return,
  .current-call-panel__control {
    transition: none;
  }

  .current-call-panel__control:hover:not(:disabled) {
    transform: none;
  }
}
</style>
