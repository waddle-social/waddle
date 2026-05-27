<script setup lang="ts">
import { computed, type Component } from "vue";
import { useStore } from "@nanostores/vue";
import { Phone, PhoneIncoming, PhoneOff, Video } from "lucide-vue-next";
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
import { $mucCallParticipants, normalizeMucCallRoomJid } from "@/lib/calls/muc-call-presence";
import { useRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import type { CallMedia } from "@/lib/calls/types";
import { barePeerJid } from "@/lib/xmpp/jid";

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
  const peerName = props.dmPeerName || peerJid.split("@")[0] || "this person";
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

function avatarInitials(label: string): string {
  const parts = label
    .trim()
    .split(/[\s._-]+/)
    .filter(Boolean);
  const initials = parts.slice(0, 2).map((part) => part.charAt(0).toUpperCase()).join("");
  return initials || "?";
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
</script>

<template>
  <div
    aria-live="polite"
    aria-atomic="true"
  >
    <section
      v-if="banner"
      role="region"
      :aria-label="banner.title"
      class="conversation-call-banner"
      :class="`conversation-call-banner--${banner.tone}`"
    >
      <div class="chat-message-lane conversation-call-banner__lane">
        <div class="conversation-call-banner__main">
          <div class="conversation-call-banner__avatars" aria-hidden="true">
            <span
              v-for="label in banner.avatarLabels.slice(0, 3)"
              :key="label"
              class="conversation-call-banner__avatar type-meta"
            >
              {{ avatarInitials(label) }}
            </span>
            <span
              v-if="banner.avatarLabels.length > 3"
              class="conversation-call-banner__avatar conversation-call-banner__avatar--more type-meta"
            >
              +{{ banner.avatarLabels.length - 3 }}
            </span>
          </div>
          <div class="conversation-call-banner__copy">
            <div class="conversation-call-banner__eyebrow type-meta">
              <span class="conversation-call-banner__pulse" aria-hidden="true" />
              <span>{{ banner.eyebrow }}</span>
            </div>
            <div class="conversation-call-banner__title-row">
              <span class="conversation-call-banner__icon" aria-hidden="true">
                <component :is="banner.icon" class="h-3.5 w-3.5" />
              </span>
              <p class="type-control truncate">
                {{ banner.title }}
              </p>
            </div>
            <p class="type-caption chat-copy-measure conversation-call-banner__body">
              {{ banner.body }}
            </p>
            <div class="conversation-call-banner__meta" aria-hidden="true">
              <span
                v-for="item in banner.meta"
                :key="item"
                class="conversation-call-banner__chip type-meta"
              >
                {{ item }}
              </span>
            </div>
          </div>
        </div>
        <div class="conversation-call-banner__actions">
          <button
            type="button"
            class="conversation-call-banner__action type-control"
            :aria-label="banner.ariaLabel"
            :disabled="banner.disabled"
            @click="activateBanner"
          >
            <component :is="banner.actionIcon" class="h-3.5 w-3.5" aria-hidden="true" />
            <span>{{ banner.actionLabel }}</span>
          </button>
          <button
            v-if="banner.secondaryAction"
            type="button"
            class="conversation-call-banner__action conversation-call-banner__action--secondary type-control"
            :aria-label="banner.secondaryAriaLabel"
            @click="activateSecondaryBanner"
          >
            <PhoneOff class="h-3.5 w-3.5" aria-hidden="true" />
            <span>{{ banner.secondaryActionLabel }}</span>
          </button>
        </div>
      </div>
    </section>
  </div>
</template>

<style scoped>
.conversation-call-banner {
  border-bottom: 1px solid color-mix(in oklab, var(--primary) 18%, var(--border));
  background:
    linear-gradient(
      90deg,
      color-mix(in oklab, var(--primary) 10%, var(--background)),
      color-mix(in oklab, var(--card) 88%, var(--primary) 12%)
    );
  color: var(--foreground);
}

.conversation-call-banner--incoming {
  border-bottom-color: color-mix(in oklab, oklch(0.76 0.17 72) 40%, var(--border));
  background:
    linear-gradient(
      90deg,
      color-mix(in oklab, oklch(0.76 0.17 72) 14%, var(--background)),
      color-mix(in oklab, var(--card) 88%, oklch(0.76 0.17 72) 12%)
    );
}

.conversation-call-banner--busy,
.conversation-call-banner--syncing {
  border-bottom-color: var(--border);
  background:
    linear-gradient(
      90deg,
      color-mix(in oklab, var(--muted) 42%, var(--background)),
      color-mix(in oklab, var(--card) 92%, var(--muted) 8%)
    );
}

.conversation-call-banner__lane {
  display: flex;
  flex-direction: column;
  gap: 0.875rem;
  padding: 0.75rem var(--chat-content-inline);
}

.conversation-call-banner__main {
  display: flex;
  min-width: 0;
  align-items: flex-start;
  gap: 0.75rem;
}

.conversation-call-banner__avatars {
  display: flex;
  flex-shrink: 0;
  align-items: center;
  padding-left: 0.25rem;
}

.conversation-call-banner__avatar {
  display: inline-flex;
  width: 2rem;
  height: 2rem;
  align-items: center;
  justify-content: center;
  margin-left: -0.25rem;
  border: 2px solid color-mix(in oklab, var(--background) 88%, transparent);
  border-radius: 9999px;
  background: color-mix(in oklab, var(--primary) 18%, var(--card));
  color: var(--primary);
  font-variant-numeric: tabular-nums;
}

.conversation-call-banner__avatar--more {
  background: color-mix(in oklab, var(--foreground) 9%, var(--card));
  color: var(--muted-foreground);
}

.conversation-call-banner__copy {
  display: flex;
  min-width: 0;
  flex: 1;
  flex-direction: column;
  gap: 0.25rem;
}

.conversation-call-banner__eyebrow,
.conversation-call-banner__title-row,
.conversation-call-banner__meta {
  display: flex;
  min-width: 0;
  align-items: center;
}

.conversation-call-banner__eyebrow {
  gap: 0.35rem;
  color: color-mix(in oklab, var(--primary) 72%, var(--foreground));
  text-transform: uppercase;
}

.conversation-call-banner--busy .conversation-call-banner__eyebrow,
.conversation-call-banner--syncing .conversation-call-banner__eyebrow {
  color: var(--muted-foreground);
}

.conversation-call-banner__pulse {
  width: 0.45rem;
  height: 0.45rem;
  border-radius: 9999px;
  background: oklch(0.68 0.2 145);
  box-shadow: 0 0 0 3px color-mix(in oklab, oklch(0.68 0.2 145) 24%, transparent);
}

.conversation-call-banner--incoming .conversation-call-banner__pulse {
  background: oklch(0.76 0.17 72);
  box-shadow: 0 0 0 3px color-mix(in oklab, oklch(0.76 0.17 72) 25%, transparent);
}

.conversation-call-banner--busy .conversation-call-banner__pulse,
.conversation-call-banner--syncing .conversation-call-banner__pulse {
  background: var(--muted-foreground);
  box-shadow: none;
}

.conversation-call-banner__title-row {
  gap: 0.4rem;
}

.conversation-call-banner__icon {
  display: inline-flex;
  flex-shrink: 0;
  color: var(--primary);
}

.conversation-call-banner__body {
  color: color-mix(in oklab, var(--foreground) 76%, transparent);
}

.conversation-call-banner__meta {
  flex-wrap: wrap;
  gap: 0.375rem;
  padding-top: 0.125rem;
}

.conversation-call-banner__chip {
  display: inline-flex;
  min-height: 1.375rem;
  align-items: center;
  border: 1px solid color-mix(in oklab, var(--primary) 18%, var(--border));
  border-radius: 9999px;
  background: color-mix(in oklab, var(--background) 74%, transparent);
  color: var(--muted-foreground);
  padding: 0 0.5rem;
  white-space: nowrap;
}

.conversation-call-banner__actions {
  display: flex;
  flex-shrink: 0;
  flex-wrap: wrap;
  gap: 0.5rem;
}

.conversation-call-banner__action {
  display: inline-flex;
  height: 2.25rem;
  flex-shrink: 0;
  align-items: center;
  justify-content: center;
  gap: 0.375rem;
  border: 1px solid color-mix(in oklab, var(--primary) 22%, var(--border));
  border-radius: 9999px;
  background: color-mix(in oklab, var(--background) 88%, transparent);
  color: var(--primary);
  padding: 0 0.875rem;
  transition: background-color 150ms ease, border-color 150ms ease, color 150ms ease;
}

.conversation-call-banner__action--secondary {
  border-color: color-mix(in oklab, var(--destructive) 24%, var(--border));
  color: var(--destructive);
}

.conversation-call-banner__action:hover {
  background: color-mix(in oklab, var(--primary) 11%, var(--background));
}

.conversation-call-banner__action--secondary:hover {
  background: color-mix(in oklab, var(--destructive) 10%, var(--background));
}

.conversation-call-banner__action:focus-visible {
  outline: none;
  box-shadow: 0 0 0 2px color-mix(in oklab, var(--primary) 35%, transparent);
}

.conversation-call-banner__action:disabled {
  cursor: not-allowed;
  border-color: var(--border);
  color: var(--muted-foreground);
}

.conversation-call-banner__action:disabled:hover {
  background: color-mix(in oklab, var(--background) 88%, transparent);
}

@media (pointer: coarse) {
  .conversation-call-banner__action {
    height: var(--control-touch);
  }
}

@media (min-width: 640px) {
  .conversation-call-banner__lane {
    flex-direction: row;
    align-items: center;
    justify-content: space-between;
  }
}
</style>
