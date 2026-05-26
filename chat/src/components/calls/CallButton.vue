<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, Video } from "lucide-vue-next";
import { $callState } from "@/lib/calls/call-store";
import { useDmCallActivity } from "@/lib/calls/dm-call-activity";
import { startDmCallAction } from "@/lib/calls/dm-call-actions";
import type { CallWireSender } from "@/lib/calls/outbound";
import { connectionStore } from "@/lib/connection-store";
import type { CallMedia } from "@/lib/calls/types";

const props = defineProps<{
  /** Peer's bare JID — the `<propose/>` per XEP-0353 §0.2 is
   *  addressed bare so every online resource is rung; whichever
   *  resource accepts becomes the responder full JID stamped on
   *  the inbound `<proceed/>`. */
  peerBareJid: string;
}>();

const state = useStore($callState);
const { activity: peerCallActivity } = useDmCallActivity(() => props.peerBareJid);

/** A call is already in flight — disable the button so the
 *  caller can't start a parallel call while one is ringing or
 *  active. Hydrated peer activity alone stays actionable: sending
 *  a fresh XEP-0353 propose lets the peer's active resource migrate
 *  the call back to this refreshed client. */
const inCall = computed(
  () => state.value.phase !== "idle" && state.value.phase !== "ended",
);

const hasPeerCallActivity = computed(() => !!peerCallActivity.value);
const voiceLabel = computed(() => hasPeerCallActivity.value && !inCall.value ? "Reconnect voice call" : "Start voice call");
const videoLabel = computed(() => hasPeerCallActivity.value && !inCall.value ? "Reconnect video call" : "Start video call");
const reconnectingPeerCall = computed(() => hasPeerCallActivity.value && !inCall.value);
const activityBannerLabel = computed(() => {
  const activity = peerCallActivity.value;
  if (!activity) return "";
  const media = activity.media.video ? "Video" : "Voice";
  if (activity.state === "accepted") return `${media} call live`;
  if (activity.direction === "incoming") return `Incoming ${media.toLowerCase()} call`;
  if (activity.direction === "outgoing") return `${media} call calling`;
  return `${media} call ringing`;
});

function getSender(): CallWireSender | null {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as CallWireSender | undefined) ?? null;
}

function getInitiator(): string | undefined {
  return (connectionStore.client as unknown as { fullJid?: string } | null)?.fullJid;
}

async function startCall(media: CallMedia): Promise<void> {
  await startDmCallAction({
    peerBareJid: props.peerBareJid,
    media,
    getSender,
    getInitiator,
  });
}
</script>

<template>
  <div
    class="call-button-group"
    :class="{ 'call-button-group--activity': reconnectingPeerCall }"
  >
    <span
      v-if="reconnectingPeerCall"
      class="call-button-group__hint type-meta"
      aria-hidden="true"
    >
      {{ activityBannerLabel }}
    </span>
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
  color: var(--success);
  text-overflow: ellipsis;
  white-space: nowrap;
}

@media (max-width: 48rem) {
  .call-button-group__hint {
    display: none;
  }
}
</style>
