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
import { $mucCallParticipants } from "@/lib/calls/muc-call-presence";
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
const participants = useStore($mucCallParticipants);
/** Other occupants currently in the call. We see our own nick
 *  echoed back in our presence broadcast, so subtract it; the
 *  indicator should read "N others are in this call", and when
 *  it's just us, the regular call button is the right CTA. */
const participantCount = computed(() => {
  const all = participants.value[props.roomJid] ?? [];
  const selfNick = connectionStore.session?.username;
  return all.filter((n) => n !== selfNick).length;
});

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
  // Our MUC nick is the session username — that's also what
  // `joinRoom` registered when this user entered the channel, so
  // it's the resource we'll use on the `<call/>` presence extension.
  const selfNick = connectionStore.session?.username ?? undefined;
  try {
    // Group call: the SFU mints the room-scoped token via
    // `<request-join/>`, the store flips to `active`, and the
    // overlay's LiveKit connect kicks in. `beginMucCall` then
    // updates our MUC presence with the `<call/>` extension so
    // other occupants see the "N in call" indicator.
    await beginMucCall(sender, props.roomJid, media, selfNick);
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
    <!-- "N in call" indicator: rendered when at least one occupant
         has advertised the call extension on their MUC presence. The
         indicator is a button-shaped chip that joins the existing
         call when clicked, so a late participant has a one-click
         affordance without us having to design a separate "join
         existing call" surface. -->
    <button
      v-if="participantCount > 0 && !inCall"
      class="chat-icon-button chat-icon-button--md text-primary hover:bg-primary/10 ring-1 ring-primary/30 motion-safe:animate-pulse"
      type="button"
      :title="`${participantCount} ${participantCount === 1 ? 'person is' : 'people are'} in this channel's call`"
      :aria-label="`Join the live call (${participantCount} in call)`"
      @click="startCall({ audio: true, video: false })"
    >
      <Phone class="w-3.5 h-3.5" />
      <span class="type-control ml-1">{{ participantCount }}</span>
    </button>
  </div>
</template>
