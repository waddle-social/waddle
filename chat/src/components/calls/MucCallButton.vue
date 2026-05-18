<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, Video } from "lucide-vue-next";
import {
  $callState,
  beginMucCall,
  reportCallError,
  type RawIqSender,
} from "@/lib/calls/call-store";
import { connectionStore } from "@/lib/connection-store";
import type { CallMedia } from "@/lib/calls/types";

const props = defineProps<{
  /** MUC room bare JID (`channel@muc.host`). The server uses this
   *  as the SFU `CallId` so every occupant who joins via this
   *  button lands in the same LiveKit room. */
  roomJid: string;
}>();

const state = useStore($callState);
const inCall = computed(() => state.value.phase !== "idle" && state.value.phase !== "ended");

function getSender(): RawIqSender | null {
  // BrowserXmppClient stores the wasm client as `xmpp`; we cast
  // through unknown because the field is private on the class.
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as RawIqSender | undefined) ?? null;
}

async function startCall(media: CallMedia): Promise<void> {
  if (inCall.value) return;
  const sender = getSender();
  if (!sender) return;
  try {
    // Group call has no propose/proceed round-trip: the SFU mints
    // the token synchronously, the store flips to `active`, and the
    // overlay's LiveKit connect kicks in. Other occupants discover
    // the call via the per-occupant `<call xmlns='urn:waddle:muc-call:0'/>`
    // presence extension (chat-side responsibility — not yet wired
    // in this PR; for the MVP the host coordinates verbally and
    // each participant clicks join in the channel).
    await beginMucCall(sender, props.roomJid, media);
  } catch (err) {
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
      title="Voice call in this channel"
      aria-label="Start voice call in this channel"
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
      title="Video call in this channel"
      aria-label="Start video call in this channel"
      :disabled="inCall"
      @click="startCall({ audio: true, video: true })"
    >
      <Video class="w-3.5 h-3.5" />
    </button>
  </div>
</template>
