<script setup lang="ts">
import { computed, ref } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, Video } from "lucide-vue-next";
import {
  $callState,
  type RawIqSender,
} from "@/lib/calls/call-store";
import { startMucCallAction } from "@/lib/calls/muc-call-actions";
import {
  $mucCallParticipants,
  normalizeMucCallRoomJid,
} from "@/lib/calls/muc-call-presence";
import { connectionStore } from "@/lib/connection-store";
import type { CallMedia } from "@/lib/calls/types";
import { jidDomain } from "@/lib/xmpp/jid";

const props = defineProps<{
  /** MUC room bare JID (`channel@muc.host`). The server uses this
   *  as the SFU `CallId` so every occupant who joins via this
   *  button lands in the same LiveKit room. */
  roomJid: string;
}>();

const state = useStore($callState);
const starting = ref(false);
const inCall = computed(() => state.value.phase !== "idle" && state.value.phase !== "ended");
const callBusy = computed(() => inCall.value || starting.value);
const participants = useStore($mucCallParticipants);
const normalizedRoomJid = computed(() => normalizeMucCallRoomJid(props.roomJid));
const callInThisRoom = computed(() => {
  const current = state.value;
  return current.phase === "active" &&
    current.kind === "muc" &&
    normalizeMucCallRoomJid(current.peer) === normalizedRoomJid.value;
});
const busyWithOtherCall = computed(() => inCall.value && !callInThisRoom.value);
/** Occupants currently in the call, including our own nick when
 *  the MUC reflects our active Muji presence. The header count must
 *  match the sidebar count so the same room doesn't appear to have
 *  different call state depending on where the user looks. */
const participantCount = computed(() => {
  return (participants.value[normalizedRoomJid.value] ?? []).length;
});

function getSender(): RawIqSender | null {
  // BrowserXmppClient stores the wasm client as `xmpp`; we cast
  // through unknown because the field is private on the class.
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as RawIqSender | undefined) ?? null;
}

function getExpectedMixerJid(): string | undefined {
  const accountJid = connectionStore.session?.jid;
  return accountJid ? `calls.${jidDomain(accountJid)}` : undefined;
}

async function startCall(media: CallMedia): Promise<void> {
  // Group call: the SFU mixer (`calls.<server-domain>`) mints
  // the room-scoped LiveKit token via a XEP-0272 Muji-bearing
  // Jingle session-initiate, publishes active Muji presence, then
  // flips the store to `active` so the overlay's LiveKit connect
  // only starts after the room-visible call indicator is valid.
  // Our MUC nick is the session username — that's also what
  // `joinRoom` registered when this user entered the channel.
  await startMucCallAction({
    roomJid: props.roomJid,
    media,
    isBusy: () => callBusy.value,
    setStarting: (next) => {
      starting.value = next;
    },
    getSender,
    getSelfNick: () => connectionStore.session?.username ?? undefined,
    getExpectedMixerJid,
  });
}
</script>

<template>
  <div class="flex items-center gap-1.5">
    <button
      class="chat-icon-button chat-icon-button--md transition-all duration-200"
      :class="callBusy
        ? 'text-muted-foreground opacity-40 cursor-not-allowed'
        : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
      type="button"
      title="Voice call in this channel"
      aria-label="Start voice call in this channel"
      :disabled="callBusy"
      @click="startCall({ audio: true, video: false })"
    >
      <Phone class="w-3.5 h-3.5" />
    </button>
    <button
      class="chat-icon-button chat-icon-button--md transition-all duration-200"
      :class="callBusy
        ? 'text-muted-foreground opacity-40 cursor-not-allowed'
        : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
      type="button"
      title="Video call in this channel"
      aria-label="Start video call in this channel"
      :disabled="callBusy"
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
      v-if="participantCount > 0"
      class="chat-icon-button chat-icon-button--md text-primary hover:bg-primary/10 ring-1 ring-primary/30 motion-safe:animate-pulse"
      type="button"
      :title="`${participantCount} ${participantCount === 1 ? 'person is' : 'people are'} in this channel's call`"
      :aria-label="`Join the live call (${participantCount} in call)`"
      :disabled="callBusy || busyWithOtherCall || callInThisRoom"
      @click="startCall({ audio: true, video: false })"
    >
      <Phone class="w-3.5 h-3.5" />
      <span class="type-control ml-1">{{ participantCount }}</span>
    </button>
  </div>
</template>
