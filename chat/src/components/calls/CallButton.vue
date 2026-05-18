<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, Video } from "lucide-vue-next";
import { $callState, beginOutgoingCall, reportCallError, scheduleOutgoingTimeout } from "@/lib/calls/call-store";
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

/** A call is already in flight — disable the button so the
 *  caller can't start a parallel call while one is ringing or
 *  active. The store tracks a single slot today (see
 *  `CallState`). */
const inCall = computed(() => state.value.phase !== "idle" && state.value.phase !== "ended");

function getSender() {
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as Parameters<typeof outboundCalls.propose>[0] | undefined) ?? null;
}

async function startCall(media: CallMedia): Promise<void> {
  if (inCall.value) return;
  const sender = getSender();
  if (!sender) return;
  const sid = newCallSid();
  beginOutgoingCall(props.peerBareJid, sid, media);
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
    reportCallError(err);
  }
}
</script>

<template>
  <div class="flex items-center gap-1.5">
    <button
      class="chat-icon-button chat-icon-button--md transition-all duration-200"
      :class="inCall
        ? 'text-muted-foreground opacity-40 cursor-not-allowed'
        : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
      type="button"
      title="Voice call"
      aria-label="Start voice call"
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
      title="Video call"
      aria-label="Start video call"
      :disabled="inCall"
      @click="startCall({ audio: true, video: true })"
    >
      <Video class="w-3.5 h-3.5" />
    </button>
  </div>
</template>
