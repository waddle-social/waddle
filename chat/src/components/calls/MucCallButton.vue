<script setup lang="ts">
import { computed, ref } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, Video } from "lucide-vue-next";
import {
  $callState,
  type RawIqSender,
} from "@/lib/calls/call-store";
import { startMucCallAction, canResumeMucCallActivity } from "@/lib/calls/muc-call-actions";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { useRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import { connectionStore } from "@/lib/connection-store";
import type { CallMedia } from "@/lib/calls/types";
import { jidDomain } from "@/lib/xmpp/jid";
import CallActivePill from "./CallActivePill.vue";

const props = withDefaults(defineProps<{
  /** MUC room bare JID (`channel@muc.host`). The server uses this
   *  as the SFU `CallId` so every occupant who joins via this
   *  button lands in the same LiveKit room. */
  roomJid: string;
  /** Keep the compact header live-call pill optional so a richer
   *  conversation-level join surface can replace it without duplicating
   *  the same action in the same view. */
  showActivePill?: boolean;
  /** When the conversation banner owns the refreshed live-call action,
   *  hide the header's "start call" controls while Muji presence says
   *  this room already has a call. */
  hideStartControlsWhenActiveCall?: boolean;
}>(), {
  showActivePill: true,
  hideStartControlsWhenActiveCall: false,
});

const state = useStore($callState);
const starting = ref(false);
const roomCall = useRoomHasActiveCall(() => props.roomJid);
const inCall = computed(() => state.value.phase !== "idle" && state.value.phase !== "ended");
const callBusy = computed(() => inCall.value || starting.value);
const showStartControls = computed(() =>
  !(props.hideStartControlsWhenActiveCall && roomCall.hasActiveCall.value)
);
const normalizedRoomJid = computed(() => normalizeMucCallRoomJid(props.roomJid));
const callInThisRoom = computed(() => {
  const current = state.value;
  return current.phase === "active" &&
    current.kind === "muc" &&
    normalizeMucCallRoomJid(current.peer) === normalizedRoomJid.value;
});
const busyWithOtherCall = computed(() => inCall.value && !callInThisRoom.value);

const pillDisabled = computed(() => callBusy.value || busyWithOtherCall.value || callInThisRoom.value);

function getSender(): RawIqSender | null {
  // BrowserXmppClient stores the wasm client as `xmpp`; we cast
  // through unknown because the field is private on the class.
  const client = connectionStore.client as unknown as { xmpp?: unknown } | null;
  return (client?.xmpp as RawIqSender | undefined) ?? null;
}

function getClientJoiner(): ((roomJid: string) => Promise<void>) | null {
  const client = connectionStore.client as unknown as {
    ensureJoined?: (roomJid: string) => Promise<void>;
  } | null;
  return client?.ensureJoined?.bind(client) ?? null;
}

function getSelfFullJid(): string | undefined {
  return connectionStore.selfFullJid ??
    (connectionStore.client as unknown as { fullJid?: string } | null)?.fullJid;
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
    getSelfFullJid,
    getExpectedMixerJid,
    ensureJoined: async () => {
      await getClientJoiner()?.(props.roomJid);
    },
    tryResumeFirst:
      roomCall.localResourceInCall.value ||
      canResumeMucCallActivity({
        roomJid: props.roomJid,
        selfFullJid: getSelfFullJid() ?? null,
      }),
  });
}

function joinExistingCall(): void {
  void startCall(roomCall.media.value);
}
</script>

<template>
  <div
    v-if="showStartControls || showActivePill"
    class="flex items-center gap-1.5"
  >
    <template v-if="showStartControls">
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
    </template>
    <!-- Live-call pill: green dot + participant count, click to
         join. The pill component decides its own visibility (only
         when this room owns the call AND the local user isn't in it),
         which is why we can mount it unconditionally and still keep
         the row spacing tight when no call is in flight. -->
    <CallActivePill
      v-if="showActivePill !== false"
      :room-jid="roomJid"
      :on-join="joinExistingCall"
      :disabled="pillDisabled"
    />
  </div>
</template>
