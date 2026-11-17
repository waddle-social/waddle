<script setup lang="ts">
import { computed } from "vue";
import { useStore } from "@nanostores/vue";
import { PhoneOff } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { btn, iconbtn, liveDot } from "styled-system/recipes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { liveKicker } from "@/ui/part-classes";
import {
  $mucCallParticipantOwners,
  $mucCallParticipants,
  mucCallParticipantCounts,
  normalizeMucCallRoomJid,
} from "@/lib/calls/muc-call-presence";
import { $callState } from "@/lib/calls/call-store";
import { $dmCallActivities, dmCallResumeBlockReason } from "@/lib/calls/dm-call-activity";
import { mucCallParticipantPreview } from "@/lib/calls/muc-call-indicators";
import {
  callActivityDockAction,
  buildCallActivityDockEntries,
  canEndRecoveredDmCallActivity,
  callActivityDockSelection,
  sortCallActivityDockEntries,
  type CallActivityDockEntry,
  type SidebarMode,
} from "@/lib/calls/call-activity-dock";
import { readRoomHasActiveCall } from "@/lib/calls/use-active-muc-call";
import type { CallMedia } from "@/lib/calls/types";
import type { ChannelSummary, GroupDmSummary } from "@/lib/chat-types";
import type { DmConversation } from "@/lib/xmpp-client";
import { barePeerJid } from "@/lib/xmpp/jid";
import { callParticipantAvatarJid } from "@/lib/avatars/author-jid";

defineOptions({ inheritAttrs: false });

const props = withDefaults(defineProps<{
  channels: ChannelSummary[];
  groupDms?: GroupDmSummary[];
  conversations: DmConversation[];
  activeChannelId: string | null;
  activeChannelRoomJid?: string | null;
  activePeerJid: string | null;
  sidebarMode: SidebarMode;
  activeChannelJids: Set<string>;
  callParticipants?: Record<string, readonly string[]>;
  callMediaByRoom?: Record<string, CallMedia>;
  managedMucDomain?: string | null;
  selfFullJid?: string | null;
  showDmCalls?: boolean;
  hideCurrentCall?: boolean;
}>(), {
  showDmCalls: true,
  hideCurrentCall: false,
  groupDms: () => [],
});

const emit = defineEmits<{
  answerDm: [peerJid: string, remoteFullJid: string, sid: string, media: CallMedia];
  selectChannel: [channelId: string | null, roomJid: string];
  selectGroupDm: [roomJid: string];
  leaveChannelCall: [roomJid: string];
  joinChannelCall: [channelId: string | null, roomJid: string, media: CallMedia];
  joinGroupDmCall: [roomJid: string, media: CallMedia];
  selectDm: [peerJid: string];
  reconnectDm: [peerJid: string, media: CallMedia];
  endDm: [peerJid: string, sid?: string];
}>();

const mucCallParticipantsStore = useStore($mucCallParticipants);
const callState = useStore($callState);
const dmCallActivities = useStore($dmCallActivities);

const callParticipants = computed(() => props.callParticipants ?? mucCallParticipantsStore.value);
const callParticipantCounts = computed<Record<string, number>>(() => {
  return mucCallParticipantCounts(callParticipants.value);
});

const entries = computed(() =>
  sortCallActivityDockEntries(
    buildCallActivityDockEntries({
      channels: props.channels,
      groupDms: props.groupDms,
      conversations: props.conversations,
      activeChannelId: props.activeChannelId,
      activeChannelRoomJid: props.activeChannelRoomJid ?? null,
      activePeerJid: props.activePeerJid,
      sidebarMode: props.sidebarMode,
      activeChannelJids: props.activeChannelJids,
      managedMucDomain: props.managedMucDomain ?? null,
      callParticipantCounts: callParticipantCounts.value,
      callParticipants: callParticipants.value,
      callMediaByRoom: props.callMediaByRoom,
      dmCallActivities: props.showDmCalls === false ? {} : dmCallActivities.value,
    }),
    callState.value,
    props.selfFullJid ?? null,
  )
);
const visibleEntries = computed(() =>
  props.hideCurrentCall
    ? entries.value.filter((entry) => !isCurrentCallEntry(entry))
    : entries.value
);
const entryCount = computed(() => visibleEntries.value.length);
const currentCallHidden = computed(() => {
  if (!props.hideCurrentCall) return false;
  const current = callState.value;
  if (current.phase === "muc-pending") return current.kind === "muc";
  if (current.phase !== "active") return false;
  if (current.kind === "muc") return true;
  return props.showDmCalls !== false && current.kind === "dm";
});
const statusMessage = computed(() => {
  const count = entryCount.value;
  const qualifier = props.showDmCalls === false ? "group " : "";
  const other = currentCallHidden.value ? "other " : "";
  if (count === 0) {
    return `No ${other}active ${qualifier}calls.`;
  }
  return `${count} ${other}active ${qualifier}${count === 1 ? "call" : "calls"}.`;
});

function isCurrentCallEntry(entry: CallActivityDockEntry): boolean {
  const current = callState.value;
  if (entry.kind === "channel") {
    if (current.phase !== "active" && current.phase !== "muc-pending") return false;
    if (current.kind !== "muc") return false;
    return normalizeMucCallRoomJid(entry.roomJid) === normalizeMucCallRoomJid(current.peer);
  }
  if (current.phase !== "active" || current.kind !== "dm") return false;
  return entry.peerJid.toLowerCase() === barePeerJid(current.peer).toLowerCase() &&
    entry.sid === current.sid;
}

function entryStatus(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const noun = entry.participantCount === 1 ? "person" : "people";
    return `${entry.participantCount} ${noun}`;
  }
  if (entry.state === "accepted") return acceptedDmEntryStatus(entry);
  if (entry.state === "ringing") {
    if (entry.direction === "outgoing") return "Calling";
    if (entry.direction === "incoming") return "Ringing";
    return "Ringing";
  }
  return "Live";
}

function acceptedDmEntryStatus(entry: Extract<CallActivityDockEntry, { kind: "dm" }>): string {
  const reason = dmCallResumeBlockReason(entry, props.selfFullJid ?? null);
  if (reason === null) return "Live";
  if (reason === "other-resource") return "Other device";
  if (reason === "expired-token" || reason === "invalid-token") {
    return entryCanEndDm(entry) ? "End available" : "Expired";
  }
  return "Details pending";
}

function entryKindLabel(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") {
    const media = entry.media.video ? "Group video call" : "Group call";
    return entry.isKnownChannel ? media : `${media} · syncing`;
  }
  if (entry.mediaKnown === false) return "Call";
  return entry.media.video ? "Video call" : "Voice call";
}

function entryMeta(entry: CallActivityDockEntry): string {
  return `${entryKindLabel(entry)} · ${entryStatus(entry)}`;
}

function entryParticipantPreview(entry: CallActivityDockEntry): string {
  if (entry.kind !== "channel") return "";
  return mucCallParticipantPreview(entry.participantLabels);
}

function entryAction(entry: CallActivityDockEntry): string {
  switch (callActivityDockAction(entry, callState.value, props.selfFullJid ?? null)) {
    case "answer":
      return "Answer";
    case "join":
      return entry.kind === "channel" && entryCanLeaveChannel(entry) ? "Rejoin" : "Join";
    case "return":
      return "Return";
    case "reconnect":
      return "Reconnect";
    case "open":
      return "Open";
  }
}

function entryTitle(entry: CallActivityDockEntry): string {
  const participants = entryParticipantPreview(entry);
  const participantPart = participants ? `, ${participants} in call` : "";
  return `${entryAction(entry)} ${entry.title}, ${entryMeta(entry)}${participantPart}`;
}

function entryCanSelect(entry: CallActivityDockEntry): boolean {
  return entry.kind !== "channel" || entry.roomJid.length > 0;
}

function entryCanEndDm(entry: CallActivityDockEntry): boolean {
  return entry.kind === "dm" &&
    canEndRecoveredDmCallActivity(entry, callState.value, props.selfFullJid ?? null);
}

function entryCanLeaveChannel(entry: CallActivityDockEntry): boolean {
  if (entry.kind !== "channel" || !entry.roomJid) return false;
  if (currentMucCallRoomJid() === normalizeMucCallRoomJid(entry.roomJid)) return false;
  return readRoomHasActiveCall(entry.roomJid).localResourceInCall;
}

function currentMucCallRoomJid(): string {
  const current = callState.value;
  if (current.phase !== "active" && current.phase !== "muc-pending") return "";
  if (current.kind !== "muc") return "";
  return normalizeMucCallRoomJid(current.peer);
}

function entryCanEnd(entry: CallActivityDockEntry): boolean {
  return entryCanEndDm(entry) || entryCanLeaveChannel(entry);
}

function endEntryLabel(entry: CallActivityDockEntry): string {
  if (entry.kind === "channel") return `Leave ${entry.title} call`;
  return endDmLabel(entry);
}

function endEntry(entry: CallActivityDockEntry): void {
  if (entry.kind === "channel" && entryCanLeaveChannel(entry)) {
    emit("leaveChannelCall", entry.roomJid);
    return;
  }
  if (entry.kind === "dm" && entryCanEndDm(entry)) {
    emit("endDm", entry.peerJid, entry.sid);
  }
}

function endDmLabel(entry: CallActivityDockEntry): string {
  if (entry.kind !== "dm") return "";
  const media = entry.mediaKnown !== false
    ? `${entry.media.video ? "video" : "voice"} call`
    : "call";
  return `End ${entry.title} ${media}`;
}

function visibleParticipantLabels(entry: CallActivityDockEntry): string[] {
  if (entry.kind !== "channel") return [];
  return entry.participantLabels.slice(0, 3);
}

const mucCallParticipantOwnersStore = useStore($mucCallParticipantOwners);

function participantAvatarJid(entry: CallActivityDockEntry, label: string): string | null {
  if (entry.kind !== "channel") return null;
  const owners = mucCallParticipantOwnersStore.value[normalizeMucCallRoomJid(entry.roomJid)] ?? [];
  return callParticipantAvatarJid(entry.roomJid, label, owners);
}


function selectEntry(entry: CallActivityDockEntry): void {
  if (!entryCanSelect(entry)) return;
  const selection = callActivityDockSelection(entry, callState.value, props.selfFullJid ?? null);
  switch (selection.kind) {
    case "dm-answer":
      emit("answerDm", selection.peerJid, selection.remoteFullJid, selection.sid, selection.media);
      return;
    case "channel-join":
      emit("joinChannelCall", selection.channelId, selection.roomJid, selection.media);
      return;
    case "group-dm-join":
      emit("joinGroupDmCall", selection.roomJid, selection.media);
      return;
    case "channel":
      emit("selectChannel", selection.channelId, selection.roomJid);
      return;
    case "group-dm":
      emit("selectGroupDm", selection.roomJid);
      return;
    case "dm-reconnect":
      emit("reconnectDm", selection.peerJid, selection.media);
      return;
    case "dm-open":
      emit("selectDm", selection.peerJid);
      return;
  }
}

// Sidebar "Live now" (Sidebar board): an aurora card per live call. The
// `call-activity-dock--mobile` hook (ChatReadyShell) turns the list into a
// swipeable strip above the page on phones, where the sidebar is a drawer.
const rootClass = css({
  display: "flex",
  flexDirection: "column",
  "&.call-activity-dock--mobile": {
    borderBottom: "1px solid {colors.border.soft}",
    bg: "bg.rail",
    padding: "{spacing.1.5} {spacing.3}",
    md: { display: "none" },
  },
});
const headClass = cx(liveKicker, css({
  display: "flex",
  alignItems: "center",
  gap: "1.5",
  padding: "{spacing.4} {spacing.2} {spacing.1} {spacing.3}",
  ".call-activity-dock--mobile &": { srOnly: true },
}));
const listClass = css({
  display: "flex",
  flexDirection: "column",
  gap: "1.5",
  margin: "0 {spacing.0.5}",
  ".call-activity-dock--mobile &": {
    flexDirection: "row",
    overflowX: "auto",
    overscrollBehaviorX: "contain",
    scrollbarWidth: "none",
    margin: 0,
    "& > *": { flex: "none", width: "min(20rem, 88vw)" },
  },
});
const cardClass = css({
  position: "relative",
  display: "flex",
  alignItems: "center",
  gap: "1",
  borderRadius: "14px",
  bg: "live.subtle",
  color: "fg",
  "&[data-active=true]": { boxShadow: "inset 0 0 0 1px {colors.live}" },
});
const rowClass = css({
  flex: 1,
  minWidth: 0,
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  padding: "{spacing.2.5} {spacing.3}",
  border: 0,
  borderRadius: "inherit",
  bg: "transparent",
  color: "inherit",
  textAlign: "left",
  cursor: "pointer",
  _hover: { "& [data-part=action]": { bg: "live.hover" } },
  // Not `_disabled`: its class name would read "disabled" in SSR output.
  "&[data-selectable=false]": { cursor: "default", opacity: 0.6 },
  _focusVisible: { outline: "2px solid {colors.focus}", outlineOffset: "2px" },
});
const copyClass = css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "1.5" });
const titleClass = css({ display: "flex", alignItems: "center", gap: "1.5", font: "700 14px/1.3 {fonts.body}", "& > span:last-child": { minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" } });
const metaClass = css({ display: "flex", alignItems: "center", gap: "1", minWidth: 0 });
const stackClass = css({ display: "inline-flex", flex: "none", "& > * + *": { marginLeft: "-6px" }, "& > *": { boxShadow: "0 0 0 2px {colors.bg.rail}", borderRadius: "pill" } });
const captionClass = css({ textStyle: "caption", color: "fg.muted", marginLeft: "1", minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const actionClass = cx(btn({ variant: "live", size: "sm" }), css({ pointerEvents: "none" }));
const hostClass = css({ display: "contents" });
const srOnlyClass = css({ srOnly: true });
const endClass = cx(iconbtn({ size: "sm" }), css({ marginRight: "2", bg: "danger", color: "danger.fg", _hover: { bg: "danger.hover", color: "danger.fg" } }));
</script>

<template>
  <div :class="hostClass">
    <p :class="srOnlyClass" role="status" aria-live="polite" aria-atomic="true">
      {{ statusMessage }}
    </p>
    <section
      v-if="visibleEntries.length > 0"
      :class="[rootClass, $attrs.class]"
      role="region"
      aria-label="Active calls"
    >
      <h2 :class="headClass">Live now <span aria-hidden="true">· {{ entryCount }}</span></h2>
      <div :class="listClass">
        <div
          v-for="entry in visibleEntries"
          :key="entry.key"
          :class="cardClass"
          :data-active="entry.isActive"
        >
          <button
            type="button"
            :class="rowClass"
            :disabled="!entryCanSelect(entry)"
            :data-selectable="entryCanSelect(entry)"
            :aria-current="entry.isActive ? 'page' : undefined"
            :title="entryTitle(entry)"
            :aria-label="entryTitle(entry)"
            @click="selectEntry(entry)"
          >
            <span :class="copyClass">
              <span :class="titleClass">
                <span :class="liveDot()" aria-hidden="true" />
                <span>{{ entry.title }}</span>
              </span>
              <span :class="metaClass">
                <span v-if="visibleParticipantLabels(entry).length" :class="stackClass" aria-hidden="true">
                  <UserAvatar
                    v-for="label in visibleParticipantLabels(entry)"
                    :key="`${entry.key}:${label}`"
                    :name="label"
                    :jid="participantAvatarJid(entry, label)"
                    :in-call="false"
                    size="sm"
                  />
                </span>
                <span :class="captionClass">{{ entryMeta(entry) }}</span>
              </span>
              <span
                v-if="entryParticipantPreview(entry)"
                :class="srOnlyClass"
              >{{ entryParticipantPreview(entry) }}</span>
            </span>
            <span :class="actionClass" data-part="action" aria-hidden="true">{{ entryAction(entry) }}</span>
          </button>
          <button
            v-if="entryCanEnd(entry)"
            type="button"
            :class="endClass"
            :title="endEntryLabel(entry)"
            :aria-label="endEntryLabel(entry)"
            @click="endEntry(entry)"
          >
            <PhoneOff aria-hidden="true" />
          </button>
        </div>
      </div>
    </section>
  </div>
</template>
