<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneIncoming, Video } from "lucide-vue-next";
import { $callState } from "@/lib/calls/call-store";
import { css, cx } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import { dmCallActivityAction } from "@/lib/calls/call-activity-dock";
import {
  hasKnownDmCallMedia,
  refreshDmCallActivityAffordances,
  useDmCallActivity,
} from "@/lib/calls/dm-call-activity";
import { answerIncomingDmCallActivity, resumeDmCallActivity } from "@/lib/calls/dm-call-actions";
import { useDmCallStart } from "@/lib/calls/use-call-start";
import type { CallWireSender } from "@/lib/calls/outbound";
import { connectionStore } from "@/lib/connection-store";

const props = withDefaults(defineProps<{
  /** Peer's bare JID — the `<propose/>` per XEP-0353 §0.2 is
   *  addressed bare so every online resource is rung; whichever
   *  resource accepts becomes the responder full JID stamped on
   *  the inbound `<proceed/>`. */
  peerBareJid: string;
  /** When the conversation owns a larger answer/reconnect banner,
   *  suppress this compact header activity affordance to avoid
   *  duplicate controls for the same XEP-0353 call activity. */
  showActivityControls?: boolean;
}>(), {
  showActivityControls: true,
});

const state = useStore($callState);
const { activity: peerCallActivity } = useDmCallActivity(() => props.peerBareJid);

/** A call is already in flight — disable the button so the
 *  caller can't start a parallel call while one is ringing or
 *  active. Hydrated peer activity only becomes a reconnect affordance
 *  when the archived LiveKit credentials still belong to this resource. */
const {
  canStart: showStartControls,
  busy: inCall,
  start: startCall,
} = useDmCallStart(() => props.peerBareJid);

const hasPeerCallActivity = computed(() => !!peerCallActivity.value);
const showPeerCallActivity = computed(() => props.showActivityControls !== false && hasPeerCallActivity.value);
const voiceLabel = computed(() => "Start voice call");
const videoLabel = computed(() => "Start video call");
const peerActivityAction = computed(() => {
  const activity = peerCallActivity.value;
  return activity ? dmCallActivityAction(activity, state.value, getInitiator() ?? null) : null;
});
const activityBannerLabel = computed(() => {
  const activity = peerCallActivity.value;
  if (!activity) return "";
  if (!hasKnownDmCallMedia(activity)) {
    if (activity.state === "accepted") return "Call live";
    if (activity.direction === "incoming") return "Incoming call";
    if (activity.direction === "outgoing") return "Calling";
    return "Call ringing";
  }
  const media = activity.media.video ? "Video" : "Voice";
  if (activity.state === "accepted") return `${media} call live`;
  if (activity.direction === "incoming") return `Incoming ${media.toLowerCase()} call`;
  if (activity.direction === "outgoing") return `Calling ${media.toLowerCase()} call`;
  return `${media} call ringing`;
});
const activityButtonLabel = computed(() => {
  const activity = peerCallActivity.value;
  if (!activity) return "";
  const media = hasKnownDmCallMedia(activity) ? (activity.media.video ? "video" : "voice") : "";
  const call = media ? `${media} call` : "call";
  switch (peerActivityAction.value) {
    case "answer":
      return `Answer ${call}`;
    case "reconnect":
      return `Reconnect ${call}`;
    case "return":
      return `Return to ${call}`;
    case "open":
      return `Open ${call}`;
    default:
      return "";
  }
});
const activityButtonDisabled = computed(() =>
  !peerActivityAction.value ||
  peerActivityAction.value === "open" ||
  peerActivityAction.value === "return",
);

function getSender(): CallWireSender | null {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as CallWireSender | undefined) ?? null;
}

function getInitiator(): string | undefined {
  return connectionStore.selfFullJid ??
    (connectionStore.client as unknown as { fullJid?: string } | null)?.fullJid;
}

async function handlePeerCallActivity(): Promise<void> {
  const activity = peerCallActivity.value;
  if (!activity) return;
  switch (peerActivityAction.value) {
    case "answer":
      if (!activity.remoteFullJid) return;
      await answerIncomingDmCallActivity({
        peerBareJid: props.peerBareJid,
        proposerFullJid: activity.remoteFullJid,
        sid: activity.sid,
        media: activity.media,
        getSender,
      });
      return;
    case "reconnect":
      if (!resumeDmCallActivity({
        peerBareJid: props.peerBareJid,
        getSelfFullJid: getInitiator,
      })) {
        refreshDmCallActivityAffordances();
      }
      return;
    case "open":
    case "return":
    default:
      return;
  }
}

// Start Voice / Video in a DM header (Huddle-DM board): plain icon buttons.
// While the peer has a call going, one live pill offers Answer / Reconnect.
const groupClass = css({
  display: "inline-flex",
  alignItems: "center",
  gap: "1",
  "&[data-activity=true]": { padding: "2px", borderRadius: "pill", bg: "live.subtle", border: "1px solid {colors.live}" },
});
const hintClass = css({
  maxWidth: "8rem",
  overflow: "hidden",
  textOverflow: "ellipsis",
  whiteSpace: "nowrap",
  paddingInline: "8px 2px",
  font: "600 13px/1 {fonts.body}",
  color: "live.text",
  _narrow: { display: "none" },
});
const buttonClass = cx(iconbtn(), css({ _disabled: { opacity: 0.4, cursor: "not-allowed" } }));
const activityClass = cx(iconbtn({ size: "sm" }), css({
  color: "live.text",
  _hover: { bg: "live", color: "live.fg" },
  _disabled: { opacity: 0.4, cursor: "not-allowed", _hover: { bg: "transparent", color: "live.text" } },
}));
</script>

<template>
  <div
    v-if="showPeerCallActivity || showStartControls"
    :class="groupClass"
    :data-activity="showPeerCallActivity"
  >
    <span
      v-if="showPeerCallActivity"
      :class="hintClass"
      aria-hidden="true"
    >
      {{ activityBannerLabel }}
    </span>
    <AppTooltip v-if="showPeerCallActivity" :label="activityButtonLabel">
      <button
        :class="activityClass"
        type="button"
        :aria-label="activityButtonLabel"
        :disabled="activityButtonDisabled"
        @click="handlePeerCallActivity"
      >
        <PhoneIncoming v-if="peerActivityAction === 'answer'" aria-hidden="true" />
        <Video v-else-if="peerCallActivity && hasKnownDmCallMedia(peerCallActivity) && peerCallActivity.media.video" aria-hidden="true" />
        <Phone v-else aria-hidden="true" />
      </button>
    </AppTooltip>
    <template v-else-if="showStartControls">
    <AppTooltip :label="voiceLabel">
      <button
        :class="buttonClass"
        type="button"
        :aria-label="voiceLabel"
        :disabled="inCall"
        @click="startCall({ audio: true, video: false })"
      >
        <Phone aria-hidden="true" />
      </button>
    </AppTooltip>
    <AppTooltip :label="videoLabel">
      <button
        :class="buttonClass"
        type="button"
        :aria-label="videoLabel"
        :disabled="inCall"
        @click="startCall({ audio: true, video: true })"
      >
        <Video aria-hidden="true" />
      </button>
    </AppTooltip>
    </template>
  </div>
</template>
