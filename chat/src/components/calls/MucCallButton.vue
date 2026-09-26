<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, Video } from "lucide-vue-next";
import { $callState } from "@/lib/calls/call-store";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { useMucCallStart } from "@/lib/calls/use-call-start";
import CallActivePill from "./CallActivePill.vue";
import AppTooltip from "@/components/ui/AppTooltip.vue";

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
const {
  canStart: showStartControls,
  busy: callBusy,
  start: startCall,
  roomCall,
} = useMucCallStart(() => props.roomJid, {
  hideWhenActiveCall: () => props.hideStartControlsWhenActiveCall,
});
const inCall = computed(() => state.value.phase !== "idle" && state.value.phase !== "ended");
const normalizedRoomJid = computed(() => normalizeMucCallRoomJid(props.roomJid));
const callInThisRoom = computed(() => {
  const current = state.value;
  return current.phase === "active" &&
    current.kind === "muc" &&
    normalizeMucCallRoomJid(current.peer) === normalizedRoomJid.value;
});
const busyWithOtherCall = computed(() => inCall.value && !callInThisRoom.value);

const pillDisabled = computed(() => callBusy.value || busyWithOtherCall.value || callInThisRoom.value);

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
    <AppTooltip label="Voice call in this channel">
      <button
        class="chat-icon-button chat-icon-button--md transition-all duration-200"
        :class="callBusy
          ? 'text-muted-foreground opacity-40 cursor-not-allowed'
          : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
        type="button"
        aria-label="Start voice call in this channel"
        :disabled="callBusy"
        @click="startCall({ audio: true, video: false })"
      >
        <Phone class="w-3.5 h-3.5" />
      </button>
    </AppTooltip>
    <AppTooltip label="Video call in this channel">
      <button
        class="chat-icon-button chat-icon-button--md transition-all duration-200"
        :class="callBusy
          ? 'text-muted-foreground opacity-40 cursor-not-allowed'
          : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
        type="button"
        aria-label="Start video call in this channel"
        :disabled="callBusy"
        @click="startCall({ audio: true, video: true })"
      >
        <Video class="w-3.5 h-3.5" />
      </button>
    </AppTooltip>
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
