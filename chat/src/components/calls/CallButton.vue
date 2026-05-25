<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, Video } from "lucide-vue-next";
import { $callState, beginOutgoingCall, reportCallError, scheduleOutgoingTimeout } from "@/lib/calls/call-store";
import { clearDmCallActivity, useDmCallActivity } from "@/lib/calls/dm-call-activity";
import { newCallSid, outboundCalls } from "@/lib/calls/outbound";
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

function getSender() {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as Parameters<typeof outboundCalls.propose>[0] | undefined) ?? null;
}

async function startCall(media: CallMedia): Promise<void> {
  if (inCall.value) return;
  const sender = getSender();
  if (!sender) return;
  const initiator = (connectionStore.client as unknown as { fullJid?: string } | null)?.fullJid;
  const sid = newCallSid();
  beginOutgoingCall(props.peerBareJid, sid, media, initiator);
  try {
    await outboundCalls.propose(sender, props.peerBareJid, sid, media);
    // Arm the auto-retract once the propose is on the wire — if we
    // armed earlier and the propose itself failed, the timer would
    // race the rollback.
    scheduleOutgoingTimeout(sender, sid);
  } catch (err) {
    // The propose stanza failed at the wire boundary. Snap the slot
    // back to idle so the UI doesn't strand on an outgoing toast
    // that never resolves, and surface the error via the global
    // call-error atom so the user knows the click did something.
    const current = $callState.get();
    if (current.phase === "outgoing" && current.sid === sid) {
      $callState.set({ phase: "idle" });
    }
    clearDmCallActivity(props.peerBareJid, sid);
    reportCallError(err);
  }
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
