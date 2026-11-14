<script setup lang="ts">
import { computed, type Component } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneIncoming, PhoneOff, Video } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { btn, liveDot, tag } from "styled-system/recipes";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { $callState } from "@/lib/calls/call-store";
import {
  canEndRecoveredDmCallActivity,
  dmCallActivityAction,
  isBusy,
} from "@/lib/calls/call-activity-dock";
import {
  $dmCallActivities,
  dmCallActivitiesForPeer,
  dmCallResumeBlockReason,
  hasKnownDmCallMedia,
  type DmCallActivity,
} from "@/lib/calls/dm-call-activity";
import { $mucCallParticipantOwners, $mucCallParticipants, normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { callParticipantAvatarJid, conversationPeerAvatarJid } from "@/lib/avatars/author-jid";
import { useAvatarUrls } from "@/lib/avatars/use-avatar-url";
import { useRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import type { CallMedia } from "@/lib/calls/types";
import { barePeerJid, jidLocalpart } from "@/lib/xmpp/jid";

const props = defineProps<{
  roomJid?: string | null;
  channelId?: string | null;
  channelName?: string | null;
  dmPeerJid?: string | null;
  dmPeerName?: string | null;
  selfFullJid?: string | null;
}>();

const emit = defineEmits<{
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  leaveChannelCall: [roomJid: string];
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  reconnectDm: [peerJid: string, media: CallMedia];
  endDm: [peerJid: string, sid?: string];
}>();

type BannerAction = "answer" | "join" | "reconnect" | "unavailable";

type BannerView = {
  kind: "group" | "dm";
  tone: "busy" | "incoming" | "live" | "syncing";
  eyebrow: string;
  title: string;
  body: string;
  meta: string[];
  avatarLabels: string[];
  action: BannerAction;
  actionLabel: string;
  ariaLabel: string;
  disabled: boolean;
  secondaryAction?: "leave" | "end-dm";
  secondaryActionLabel?: string;
  secondaryAriaLabel?: string;
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
const mucCallParticipants = useStore($mucCallParticipants);
const roomCall = useRoomHasActiveCall(() => props.roomJid ?? "");

const normalizedRoomJid = computed(() => normalizeMucCallRoomJid(props.roomJid ?? ""));
const normalizedDmPeerJid = computed(() => barePeerJid(props.dmPeerJid ?? "").toLowerCase());
const roomParticipantLabels = computed(() => {
  const roomJid = normalizedRoomJid.value;
  if (!roomJid) return [];
  return mucCallParticipants.value[roomJid] ?? [];
});
const dmActivity = computed<DmCallActivity | null>(() => {
  const peerJid = normalizedDmPeerJid.value;
  if (!peerJid) return null;
  return dmCallActivitiesForPeer(dmCallActivities.value, peerJid, props.selfFullJid ?? null)[0] ?? null;
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

const mucCallParticipantOwners = useStore($mucCallParticipantOwners);

/** Avatar JID behind a banner label: the DM peer, or a call participant's real JID. */
function avatarJidFor(label: string): string | null {
  if (banner.value?.kind === "dm") {
    return props.dmPeerJid ? conversationPeerAvatarJid(props.dmPeerJid) : null;
  }
  const roomJid = normalizedRoomJid.value;
  return callParticipantAvatarJid(roomJid, label, mucCallParticipantOwners.value[roomJid] ?? []);
}

const avatarUrlFor = useAvatarUrls(() => (banner.value?.avatarLabels.slice(0, 3) ?? []).map(avatarJidFor));

function groupCallBanner(): BannerView | null {
  const roomJid = normalizedRoomJid.value;
  if (!roomJid || props.dmPeerJid) return null;
  if (!roomCall.hasActiveCall.value || localGroupCallInThisRoom.value) return null;

  const participantCount = roomCall.participantCount.value;
  const participantNoun = participantCount === 1 ? "person" : "people";
  const title = `${props.channelName || "This channel"} has a live call`;
  const busy = isBusy(callState.value);
  const retainedLocalResource = roomCall.localResourceInCall.value && !localGroupCallInThisRoom.value;
  const media = roomCall.media.value;
  const mediaLabel = media.video ? "Video call" : "Voice call";

  return {
    kind: "group",
    tone: busy ? "busy" : retainedLocalResource ? "syncing" : "live",
    eyebrow: retainedLocalResource ? "Recovered after refresh" : "Live now",
    title,
    body: retainedLocalResource
      ? "This browser is still listed in the call after reload. Rejoin media or leave to clear your call presence."
      : `${participantCount} ${participantNoun} connected in this channel.`,
    meta: retainedLocalResource ? [mediaLabel, "This browser"] : [mediaLabel, "Channel"],
    avatarLabels: roomParticipantLabels.value,
    action: busy ? "unavailable" : "join",
    actionLabel: busy ? "In another call" : retainedLocalResource ? "Rejoin call" : "Join call",
    ariaLabel: busy
      ? `${title}; already in another call`
      : retainedLocalResource
        ? `Rejoin ${props.channelName || "channel"} call after refresh`
        : `Join ${props.channelName || "channel"} call`,
    disabled: busy,
    ...(retainedLocalResource ? {
      secondaryAction: "leave" as const,
      secondaryActionLabel: "Leave call",
      secondaryAriaLabel: `Leave ${props.channelName || "channel"} call after refresh`,
    } : {}),
    icon: media.video ? Video : Phone,
    actionIcon: busy ? PhoneOff : media.video ? Video : Phone,
    channelId: props.channelId ?? null,
    roomJid,
    media,
  };
}

function dmBanner(): BannerView | null {
  const activity = dmActivity.value;
  const peerJid = normalizedDmPeerJid.value;
  if (!activity || !peerJid || props.roomJid) return null;

  const action = dmCallActivityAction(activity, callState.value, props.selfFullJid ?? null);
  if (action === "return") return null;

  const media = activity.media;
  const peerName = props.dmPeerName || jidLocalpart(peerJid) || "this person";
  const mediaLabel = media.video ? "video call" : "voice call";
  const title = dmTitle(activity, peerName);
  const busy = isBusy(callState.value);

  if (action === "answer" && activity.remoteFullJid) {
    return {
      kind: "dm",
      tone: "incoming",
      eyebrow: "Incoming call",
      title,
      body: `${peerName} is calling with a ${mediaLabel}.`,
      meta: [capitalizeLabel(mediaLabel), "Ringing"],
      avatarLabels: [peerName],
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

  const canEndRecoveredCall = canEndRecoveredDmCallActivity(
    activity,
    callState.value,
    props.selfFullJid ?? null,
  );

  if (action === "reconnect" && hasKnownDmCallMedia(activity)) {
    return {
      kind: "dm",
      tone: "live",
      eyebrow: canEndRecoveredCall ? "Recovered after refresh" : "Live now",
      title,
      body: canEndRecoveredCall
        ? `The ${mediaLabel} is still live after reload. Rejoin media or end it from this tab.`
        : `The ${mediaLabel} is still live.`,
      meta: [capitalizeLabel(mediaLabel), "1:1"],
      avatarLabels: [peerName],
      action: "reconnect",
      actionLabel: "Rejoin",
      ariaLabel: `Rejoin ${peerName} ${mediaLabel}`,
      disabled: false,
      ...(canEndRecoveredCall ? {
        secondaryAction: "end-dm" as const,
        secondaryActionLabel: "End call",
        secondaryAriaLabel: `End ${peerName} ${mediaLabel} after refresh`,
      } : {}),
      icon: media.video ? Video : Phone,
      actionIcon: media.video ? Video : Phone,
      peerJid,
      sid: activity.sid,
      media,
    };
  }

  if (canEndRecoveredCall && activity.state === "accepted") {
    return {
      kind: "dm",
      tone: "syncing",
      eyebrow: "Recovered after refresh",
      title,
      body: `The saved reconnect details for this ${mediaLabel} expired, but this tab can still end the call.`,
      meta: [capitalizeLabel(mediaLabel), "1:1"],
      avatarLabels: [peerName],
      action: "unavailable",
      actionLabel: "Unavailable",
      ariaLabel: `${title}; reconnect details expired`,
      disabled: true,
      secondaryAction: "end-dm",
      secondaryActionLabel: "End call",
      secondaryAriaLabel: `End ${peerName} ${mediaLabel} after refresh`,
      icon: media.video ? Video : Phone,
      actionIcon: PhoneOff,
      peerJid,
      sid: activity.sid,
      media,
    };
  }

  const unavailable = dmUnavailableState(activity, mediaLabel, title, busy);
  return {
    kind: "dm",
    tone: unavailable.tone,
    eyebrow: unavailable.eyebrow,
    title,
    body: unavailable.body,
    meta: [capitalizeLabel(mediaLabel), unavailable.meta],
    avatarLabels: [peerName],
    action: "unavailable",
    actionLabel: unavailable.actionLabel,
    ariaLabel: unavailable.ariaLabel,
    disabled: true,
    icon: Phone,
    actionIcon: PhoneOff,
  };
}

function dmUnavailableState(
  activity: DmCallActivity,
  mediaLabel: string,
  title: string,
  busy: boolean,
): {
  tone: "busy" | "syncing";
  eyebrow: string;
  body: string;
  meta: string;
  actionLabel: string;
  ariaLabel: string;
} {
  if (busy) {
    return {
      tone: "busy",
      eyebrow: "Busy",
      body: `Finish the current call before joining this ${mediaLabel}.`,
      meta: "Another call",
      actionLabel: "In another call",
      ariaLabel: `${title}; already in another call`,
    };
  }

  if (activity.state === "accepted") {
    const reason = dmCallResumeBlockReason(activity, props.selfFullJid ?? null);
    if (reason === "missing-join" || reason === "missing-self") {
      return {
        tone: "syncing",
        eyebrow: "Syncing",
        body: "Reconnect details are not available on this tab yet.",
        meta: "Details pending",
        actionLabel: "Unavailable",
        ariaLabel: `${title}; reconnect details unavailable`,
      };
    }
    if (reason === "other-resource") {
      return {
        tone: "syncing",
        eyebrow: "Other device",
        body: "This call is live on another browser or device.",
        meta: "Other device",
        actionLabel: "Unavailable",
        ariaLabel: `${title}; live on another device`,
      };
    }
    if (reason === "expired-token" || reason === "invalid-token") {
      return {
        tone: "syncing",
        eyebrow: "Expired",
        body: "The saved reconnect details expired.",
        meta: "Expired",
        actionLabel: "Unavailable",
        ariaLabel: `${title}; reconnect details expired`,
      };
    }
  }

  if (activity.state === "ringing" && activity.direction === "outgoing") {
    return {
      tone: "syncing",
      eyebrow: "Calling",
      body: `The ${mediaLabel} is still ringing.`,
      meta: "Ringing",
      actionLabel: "Waiting",
      ariaLabel: `${title}; still ringing`,
    };
  }

  return {
    tone: "syncing",
    eyebrow: "Syncing",
    body: "Waiting for call details to finish syncing.",
    meta: "Details pending",
    actionLabel: "Unavailable",
    ariaLabel: `${title}; call details unavailable`,
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

function capitalizeLabel(label: string): string {
  return label.charAt(0).toUpperCase() + label.slice(1);
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

function activateSecondaryBanner(): void {
  const current = banner.value;
  if (!current) return;
  if (current.secondaryAction === "leave" && current.roomJid) {
    emit("leaveChannelCall", current.roomJid);
    return;
  }
  if (current.secondaryAction === "end-dm" && current.peerJid) {
    emit("endDm", current.peerJid, current.sid);
  }
}

// A live call in this conversation (Huddle-Join board): a card above the
// timeline with who is in it and Join / Leave. `data-tone` quiets it when you
// are busy elsewhere or the call is still syncing.
const laneClass = css({ padding: "12px 24px 0", _narrow: { padding: "8px 12px 0" } });
const cardClass = css({
  display: "flex",
  flexWrap: "wrap",
  alignItems: "center",
  gap: "3",
  maxWidth: "read",
  padding: "4",
  borderRadius: "16px",
  bg: "surface",
  border: "1px solid {colors.border.soft}",
  color: "fg",
  "&[data-tone=incoming]": { borderColor: "live" },
});
const mainClass = css({ flex: "1 1 280px", minWidth: 0, display: "flex", alignItems: "center", gap: "2.5" });
const iconClass = css({
  position: "relative",
  display: "inline-flex",
  flex: "none",
  width: "36px",
  height: "36px",
  alignItems: "center",
  justifyContent: "center",
  borderRadius: "pill",
  bg: "live.subtle",
  color: "live.text",
  "& svg": { width: "18px", height: "18px" },
  "[data-tone=busy] &, [data-tone=syncing] &": { bg: "surface.2", color: "fg.muted" },
});
const iconDotClass = cx(liveDot(), css({ position: "absolute", top: 0, right: 0, width: "10px", height: "10px", boxShadow: "0 0 0 2px {colors.surface}", "[data-tone=busy] &, [data-tone=syncing] &": { display: "none" } }));
const copyClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5", lineHeight: "1.35" });
const eyebrowClass = css({ textStyle: "eyebrow", color: "live.text", "[data-tone=busy] &, [data-tone=syncing] &": { color: "fg.muted" } });
const titleClass = css({ margin: 0, font: "700 15px/1.35 {fonts.body}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const bodyClass = css({ margin: 0, textStyle: "caption", color: "fg.muted" });
const metaClass = css({ display: "flex", flexWrap: "wrap", alignItems: "center", gap: "1.5", marginTop: "1" });
const avatarsClass = css({ display: "inline-flex", "& > * + *": { marginLeft: "-6px" }, "& > *": { boxShadow: "0 0 0 2px {colors.surface}", borderRadius: "pill" } });
const moreClass = css({ display: "inline-flex", alignItems: "center", justifyContent: "center", height: "28px", minWidth: "28px", px: "1.5", borderRadius: "pill", bg: "surface.2", color: "fg.muted", font: "700 12px/1 {fonts.body}" });
const actionsClass = css({ display: "flex", flexWrap: "wrap", gap: "2", marginLeft: "auto" });
const primaryClass = cx(btn({ variant: "live", size: "sm" }), css({ _coarse: { height: "44px" } }));
const secondaryClass = cx(btn({ size: "sm" }), css({ _coarse: { height: "44px" } }));
</script>

<template>
  <div
    aria-live="polite"
    aria-atomic="true"
  >
    <div v-if="banner" :class="laneClass">
      <section
        role="region"
        :aria-label="banner.title"
        :class="cardClass"
        :data-tone="banner.tone"
      >
        <div :class="mainClass">
          <span :class="iconClass" aria-hidden="true">
            <component :is="banner.icon" />
            <span :class="iconDotClass" />
          </span>
          <div :class="copyClass">
            <span :class="eyebrowClass">{{ banner.eyebrow }}</span>
            <p :class="titleClass">{{ banner.title }}</p>
            <p :class="bodyClass">{{ banner.body }}</p>
            <div :class="metaClass" aria-hidden="true">
              <span v-if="banner.avatarLabels.length" :class="avatarsClass">
                <AppAvatar
                  v-for="label in banner.avatarLabels.slice(0, 3)"
                  :key="label"
                  :name="label"
                  :src="avatarUrlFor(avatarJidFor(label))"
                  size="sm"
                />
                <span v-if="banner.avatarLabels.length > 3" :class="moreClass">
                  +{{ banner.avatarLabels.length - 3 }}
                </span>
              </span>
              <span v-for="item in banner.meta" :key="item" :class="tag()">{{ item }}</span>
            </div>
          </div>
        </div>
        <div :class="actionsClass">
          <button
            type="button"
            :class="primaryClass"
            :aria-label="banner.ariaLabel"
            :disabled="banner.disabled"
            @click="activateBanner"
          >
            <component :is="banner.actionIcon" aria-hidden="true" />
            <span>{{ banner.actionLabel }}</span>
          </button>
          <button
            v-if="banner.secondaryAction"
            type="button"
            :class="secondaryClass"
            :aria-label="banner.secondaryAriaLabel"
            @click="activateSecondaryBanner"
          >
            <PhoneOff aria-hidden="true" />
            <span>{{ banner.secondaryActionLabel }}</span>
          </button>
        </div>
      </section>
    </div>
  </div>
</template>
