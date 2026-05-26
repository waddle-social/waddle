<script setup lang="ts">
import { computed, type Component } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneIncoming, PhoneOff, Video } from "lucide-vue-next";
import { $callState } from "@/lib/calls/call-store";
import { dmCallActivityAction, isBusy } from "@/lib/calls/call-activity-dock";
import { $dmCallActivities, hasKnownDmCallMedia, type DmCallActivity } from "@/lib/calls/dm-call-activity";
import { normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { useRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import type { CallMedia } from "@/lib/calls/types";
import { barePeerJid } from "@/lib/xmpp/jid";

const props = defineProps<{
  roomJid?: string | null;
  channelId?: string | null;
  channelName?: string | null;
  dmPeerJid?: string | null;
  dmPeerName?: string | null;
}>();

const emit = defineEmits<{
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  reconnectDm: [peerJid: string, media: CallMedia];
}>();

type BannerAction = "answer" | "join" | "reconnect" | "unavailable";

type BannerView = {
  kind: "group" | "dm";
  title: string;
  body: string;
  action: BannerAction;
  actionLabel: string;
  ariaLabel: string;
  disabled: boolean;
  icon: Component;
  actionIcon: Component;
  channelId?: string | null;
  roomJid?: string;
  peerJid?: string;
  remoteFullJid?: string;
  sid?: string;
  media?: CallMedia;
};

const callState = useStore($callState);
const dmCallActivities = useStore($dmCallActivities);
const roomCall = useRoomHasActiveCall(() => props.roomJid ?? "");

const normalizedRoomJid = computed(() => normalizeMucCallRoomJid(props.roomJid ?? ""));
const normalizedDmPeerJid = computed(() => barePeerJid(props.dmPeerJid ?? "").toLowerCase());
const dmActivity = computed<DmCallActivity | null>(() => {
  const peerJid = normalizedDmPeerJid.value;
  if (!peerJid) return null;
  return dmCallActivities.value[peerJid] ?? null;
});

const localGroupCallInThisRoom = computed(() => {
  const roomJid = normalizedRoomJid.value;
  const state = callState.value;
  if (!roomJid) return false;
  if (state.phase !== "active" && state.phase !== "muc-pending") return false;
  if (state.kind !== "muc") return false;
  return normalizeMucCallRoomJid(state.peer) === roomJid;
});

const banner = computed<BannerView | null>(() => {
  const groupBanner = groupCallBanner();
  if (groupBanner) return groupBanner;
  return dmBanner();
});

function groupCallBanner(): BannerView | null {
  const roomJid = normalizedRoomJid.value;
  if (!roomJid || props.dmPeerJid) return null;
  if (!roomCall.hasActiveCall.value || localGroupCallInThisRoom.value) return null;

  const participantCount = roomCall.participantCount.value;
  const participantNoun = participantCount === 1 ? "person" : "people";
  const title = `${props.channelName || "This channel"} has a live call`;
  const busy = isBusy(callState.value);

  return {
    kind: "group",
    title,
    body: `${participantCount} ${participantNoun} connected in this channel.`,
    action: busy ? "unavailable" : "join",
    actionLabel: busy ? "In another call" : "Join call",
    ariaLabel: busy ? `${title}; already in another call` : `Join ${props.channelName || "channel"} call`,
    disabled: busy,
    icon: Phone,
    actionIcon: busy ? PhoneOff : Phone,
    channelId: props.channelId ?? null,
    roomJid,
    media: { audio: true, video: false },
  };
}

function dmBanner(): BannerView | null {
  const activity = dmActivity.value;
  const peerJid = normalizedDmPeerJid.value;
  if (!activity || !peerJid || props.roomJid) return null;

  const action = dmCallActivityAction(activity, callState.value);
  if (action === "return") return null;

  const media = activity.media;
  const peerName = props.dmPeerName || peerJid.split("@")[0] || "this person";
  const mediaLabel = media.video ? "video call" : "voice call";
  const title = dmTitle(activity, peerName);
  const busy = isBusy(callState.value);

  if (action === "answer" && activity.remoteFullJid) {
    return {
      kind: "dm",
      title,
      body: `${peerName} is calling with a ${mediaLabel}.`,
      action: "answer",
      actionLabel: "Answer",
      ariaLabel: `Answer ${peerName} ${mediaLabel}`,
      disabled: false,
      icon: PhoneIncoming,
      actionIcon: media.video ? Video : PhoneIncoming,
      peerJid,
      remoteFullJid: activity.remoteFullJid,
      sid: activity.sid,
      media,
    };
  }

  if (action === "reconnect" && hasKnownDmCallMedia(activity)) {
    return {
      kind: "dm",
      title,
      body: `The ${mediaLabel} is still live after refresh.`,
      action: "reconnect",
      actionLabel: "Rejoin",
      ariaLabel: `Rejoin ${peerName} ${mediaLabel}`,
      disabled: false,
      icon: media.video ? Video : Phone,
      actionIcon: media.video ? Video : Phone,
      peerJid,
      media,
    };
  }

  return {
    kind: "dm",
    title,
    body: busy ? `Finish the current call before joining this ${mediaLabel}.` : "Waiting for call details to finish syncing.",
    action: "unavailable",
    actionLabel: busy ? "In another call" : "Unavailable",
    ariaLabel: busy ? `${title}; already in another call` : `${title}; call details unavailable`,
    disabled: true,
    icon: Phone,
    actionIcon: PhoneOff,
  };
}

function dmTitle(activity: DmCallActivity, peerName: string): string {
  if (activity.state === "ringing" && activity.direction === "incoming") {
    return `${peerName} is calling`;
  }
  if (activity.state === "ringing" && activity.direction === "outgoing") {
    return `Calling ${peerName}`;
  }
  return `Call with ${peerName} is live`;
}

function activateBanner(): void {
  const current = banner.value;
  if (!current || current.disabled) return;
  if (current.action === "join" && current.roomJid && current.media) {
    emit("joinChannelCall", current.channelId ?? null, current.roomJid, current.media);
    return;
  }
  if (
    current.action === "answer" &&
    current.peerJid &&
    current.remoteFullJid &&
    current.sid &&
    current.media
  ) {
    emit("answerDm", current.peerJid, current.remoteFullJid, current.sid, current.media);
    return;
  }
  if (current.action === "reconnect" && current.peerJid && current.media) {
    emit("reconnectDm", current.peerJid, current.media);
  }
}
</script>

<template>
  <section
    role="region"
    :aria-label="banner ? banner.title : 'Conversation call status'"
    aria-live="polite"
    aria-atomic="true"
  >
    <div
      v-if="banner"
      class="border-b border-primary/15 bg-primary/8 text-foreground"
    >
      <div class="chat-message-lane flex flex-col gap-3 px-[var(--chat-content-inline)] py-3 sm:flex-row sm:items-center sm:justify-between">
        <div class="flex min-w-0 items-start gap-3">
          <div class="mt-0.5 flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-full border border-primary/20 bg-background/80 text-primary">
            <component :is="banner.icon" class="h-4 w-4" aria-hidden="true" />
          </div>
          <div class="min-w-0">
            <p class="type-control truncate">
              {{ banner.title }}
            </p>
            <p class="type-caption chat-copy-measure text-foreground/75">
              {{ banner.body }}
            </p>
          </div>
        </div>
        <button
          type="button"
          class="type-control inline-flex h-8 shrink-0 items-center justify-center gap-1.5 rounded-full border border-primary/20 bg-background/85 px-3.5 text-primary transition-colors hover:bg-primary/10 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/35 disabled:cursor-not-allowed disabled:border-border disabled:text-muted-foreground disabled:hover:bg-background/85"
          :aria-label="banner.ariaLabel"
          :disabled="banner.disabled"
          @click="activateBanner"
        >
          <component :is="banner.actionIcon" class="h-3.5 w-3.5" aria-hidden="true" />
          <span>{{ banner.actionLabel }}</span>
        </button>
      </div>
    </div>
  </section>
</template>
