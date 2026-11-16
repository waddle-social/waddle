<script setup lang="ts">
import { computed, ref } from "vue";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import { css, cx } from "styled-system/css";
import { btn, iconbtn, liveDot } from "styled-system/recipes";
import { useStore } from "@nanostores/vue";
import {
  Maximize2,
  Mic,
  MicOff,
  PhoneOff,
  Settings,
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
import { useActiveMucCall } from "@/lib/calls/use-active-muc-call";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";
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
const { selfInCall } = useActiveMucCall();

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
      title: channel?.name ?? jidLocalpart(roomJid),
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
    title: conversation?.peerUsername ?? jidLocalpart(peerJid),
    targetJid: peerJid,
    channelId: null,
    media: current.media,
    connected: true,
    viewing: barePeerJid(props.activePeerJid ?? "").toLowerCase() === peerJid,
    participantCount: 2,
    participantLabels: [],
  };
});

// The panel is the way back to a call you are not looking at. While the
// call's own conversation is open, its stage (split, expanded or fullscreen)
// already carries every control, so a second bar would only duplicate them.
// Mirrors the stage's render gate in CallSplitContainer / CallExpandedSurface.
const stageOnScreen = computed(() => {
  const current = panel.value;
  if (!current?.viewing || state.value.phase !== "active") return false;
  return current.kind === "dm" || selfInCall.value;
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

// "Your huddle" (Sidebar / Huddle-Minimized boards): the call you are in,
// docked at the bottom of the sidebar so it follows you everywhere. The
// `current-call-panel--mobile` hook (ChatReadyShell) makes it the full-bleed
// bar above the page on phones (Mobile-Huddle, minimized).
const rootClass = css({
  display: "flex",
  flexDirection: "column",
  gap: "2",
  padding: "10px 12px",
  borderRadius: "14px",
  border: "1px solid {colors.live}",
  bg: "live.subtle",
  color: "fg",
  "&.current-call-panel--mobile": {
    borderRadius: 0,
    border: 0,
    borderBottom: "1px solid {colors.live}",
    md: { display: "none" },
  },
});
const summaryClass = css({ display: "flex", alignItems: "center", gap: "2.5", minWidth: 0 });
const dotClass = cx(liveDot(), css({ "&[data-connecting=true]": { bg: "fg.soft", animation: "none" } }));
const copyClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", lineHeight: "1.3" });
const titleClass = css({ font: "700 14px/1.3 {fonts.body}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const statusClass = css({ textStyle: "caption", color: "live.text", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const returnClass = cx(btn({ size: "sm" }), css({ flex: "none", _disabled: { opacity: 1, cursor: "default", bg: "transparent", borderColor: "transparent", color: "live.text" } }));
const controlsClass = css({ display: "flex", alignItems: "center", gap: "1.5" });
const controlClass = cx(iconbtn({ size: "sm" }), css({
  bg: "surface",
  color: "fg",
  _coarse: { width: "44px", height: "44px" },
  "&[data-off=true]": { bg: "color-mix(in srgb, {colors.danger} 18%, transparent)", color: "danger.text" },
  _disabled: { opacity: 0.5, cursor: "not-allowed" },
}));
const hangupClass = cx(iconbtn({ size: "sm" }), css({
  marginLeft: "auto",
  bg: "danger",
  color: "danger.fg",
  _hover: { bg: "danger.hover", color: "danger.fg" },
  _coarse: { width: "44px", height: "44px" },
}));
</script>

<template>
  <aside
    v-if="panel && !stageOnScreen"
    :class="rootClass"
    aria-label="Current call"
  >
    <div :class="summaryClass">
      <span :class="dotClass" :data-connecting="!isConnected" aria-hidden="true" />
      <span :class="copyClass">
        <span :class="titleClass">{{ panel.title }}</span>
        <span :class="statusClass">{{ statusLabel }} · {{ callTypeLabel }} · {{ participantLabel }}</span>
      </span>
      <button
        type="button"
        :class="returnClass"
        :disabled="panel.viewing"
        :aria-label="`${returnLabel} ${panel.title} call`"
        @click="returnToCall"
      >
        <span>{{ returnLabel }}</span>
      </button>
    </div>

    <div :class="controlsClass" aria-label="Current call controls">
      <AppTooltip :label="micEnabled ? 'Mute' : 'Unmute'">
        <button
          type="button"
          :class="controlClass"
          :data-off="!micEnabled"
          :disabled="controlsDisabled"
          :aria-label="micEnabled ? 'Mute microphone' : 'Unmute microphone'"
          @click="toggleMic"
        >
          <Mic v-if="micEnabled" aria-hidden="true" />
          <MicOff v-else aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip :label="camEnabled ? 'Camera off' : 'Camera on'">
        <button
          type="button"
          :class="controlClass"
          :disabled="controlsDisabled"
          :aria-label="camEnabled ? 'Turn camera off' : 'Turn camera on'"
          @click="toggleCam"
        >
          <Video v-if="camEnabled" aria-hidden="true" />
          <VideoOff v-else aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip label="Expand call">
        <button
          type="button"
          :class="controlClass"
          :disabled="controlsDisabled"
          aria-label="Expand current call"
          @click="expandCall"
        >
          <Maximize2 aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip label="Call settings">
        <button
          type="button"
          :class="controlClass"
          :disabled="controlsDisabled"
          aria-label="Open call settings"
          @click="settingsOpen = true"
        >
          <Settings aria-hidden="true" />
        </button>
      </AppTooltip>
      <AppTooltip label="Hang up">
        <button
          type="button"
          :class="hangupClass"
          aria-label="Hang up current call"
          @click="hangupActiveCall"
        >
          <PhoneOff aria-hidden="true" />
        </button>
      </AppTooltip>
    </div>

    <CallSettingsDialog v-model:open="settingsOpen" />
  </aside>
</template>
