<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneIncoming, Video } from "lucide-vue-next";
import { $callState } from "@/lib/calls/call-store";
import { dmCallActivityAction } from "@/lib/calls/call-activity-dock";
import {
  hasKnownDmCallMedia,
  refreshDmCallActivityAffordances,
  useDmCallActivity,
} from "@/lib/calls/dm-call-activity";
import { answerIncomingDmCallActivity, resumeDmCallActivity, startDmCallAction } from "@/lib/calls/dm-call-actions";
import type { CallWireSender } from "@/lib/calls/outbound";
import { connectionStore } from "@/lib/connection-store";
import type { CallMedia } from "@/lib/calls/types";

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
const inCall = computed(
  () => state.value.phase !== "idle" && state.value.phase !== "ended",
);

const hasPeerCallActivity = computed(() => !!peerCallActivity.value);
const showPeerCallActivity = computed(() => props.showActivityControls !== false && hasPeerCallActivity.value);
const showStartControls = computed(() => !hasPeerCallActivity.value);
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

async function startCall(media: CallMedia): Promise<void> {
  await startDmCallAction({
    peerBareJid: props.peerBareJid,
    media,
    getSender,
    getInitiator,
  });
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
</script>

<template>
  <div
    v-if="showPeerCallActivity || showStartControls"
    class="call-button-group"
    :class="{ 'call-button-group--activity': showPeerCallActivity }"
  >
    <span
      v-if="showPeerCallActivity"
      class="call-button-group__hint type-meta"
      aria-hidden="true"
    >
      {{ activityBannerLabel }}
    </span>
    <button
      v-if="showPeerCallActivity"
      class="chat-icon-button chat-icon-button--md transition-all duration-200"
      :class="activityButtonDisabled
        ? 'text-muted-foreground opacity-40 cursor-not-allowed'
        : 'text-success-foreground hover:bg-success/10 hover:text-success-foreground'"
      type="button"
      :title="activityButtonLabel"
      :aria-label="activityButtonLabel"
      :disabled="activityButtonDisabled"
      @click="handlePeerCallActivity"
    >
      <PhoneIncoming v-if="peerActivityAction === 'answer'" class="w-3.5 h-3.5" />
      <Video v-else-if="peerCallActivity && hasKnownDmCallMedia(peerCallActivity) && peerCallActivity.media.video" class="w-3.5 h-3.5" />
      <Phone v-else class="w-3.5 h-3.5" />
    </button>
    <template v-else-if="showStartControls">
    <button
      class="chat-icon-button chat-icon-button--md transition-all duration-200"
      :class="inCall
        ? 'text-muted-foreground opacity-40 cursor-not-allowed'
        : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
      type="button"
      :title="voiceLabel"
      :aria-label="voiceLabel"
      :disabled="inCall"
      @click="startCall({ audio: true, video: false })"
    >
      <Phone class="w-3.5 h-3.5" />
    </button>
    <button
      class="chat-icon-button chat-icon-button--md transition-all duration-200"
      :class="inCall
        ? 'text-muted-foreground opacity-40 cursor-not-allowed'
        : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
      type="button"
      :title="videoLabel"
      :aria-label="videoLabel"
      :disabled="inCall"
      @click="startCall({ audio: true, video: true })"
    >
      <Video class="w-3.5 h-3.5" />
    </button>
    </template>
  </div>
</template>

<style scoped>
.call-button-group {
  display: inline-flex;
  align-items: center;
  gap: 0.375rem;
}

.call-button-group--activity {
  gap: 0.25rem;
  border: 1px solid color-mix(in oklab, var(--success) 28%, transparent);
  border-radius: 9999px;
  background: color-mix(in oklab, var(--success) 10%, transparent);
  padding: 0.125rem;
}

.call-button-group__hint {
  max-width: 8rem;
  overflow: hidden;
  padding-inline: 0.375rem 0.125rem;
  color: var(--success-foreground);
  text-overflow: ellipsis;
  white-space: nowrap;
}

@media (max-width: 48rem) {
  .call-button-group__hint {
    display: none;
  }
}
</style>
