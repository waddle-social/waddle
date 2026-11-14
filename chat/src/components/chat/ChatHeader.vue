<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, type Component } from "vue";
import { ChevronLeft, Hash, MessageCircle, MessagesSquare, PhoneCall, Pin, Search, Settings, Users } from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { header, iconbtn, tag } from "styled-system/recipes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { conversationPeerAvatarJid } from "@/lib/avatars/author-jid";
import AppTooltip from "@/components/ui/AppTooltip.vue";
import CallButton from "@/components/calls/CallButton.vue";
import MucCallButton from "@/components/calls/MucCallButton.vue";
import ChatHeaderOverflowMenu from "@/components/chat/ChatHeaderOverflowMenu.vue";
import NotifyModeButton from "@/components/chat/NotifyModeButton.vue";
import { hasKnownDmCallMedia, useDmCallActivity } from "@/lib/calls/dm-call-activity";
import { useDmCallStart, useMucCallStart } from "@/lib/calls/use-call-start";
import { formatIdle } from "@/presence/idle-duration";
import { useInCallOverlay } from "@/presence/in-call-overlay-store";
import type { ChannelSummary, SpaceSummary } from "@/lib/chat-types";
import type { ConnectionNoticeCopy } from "@/lib/connection-notice";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { OccupantPresence } from "@/lib/xmpp-client";
import type { NotifySettingsStore } from "@/lib/notify-settings";
import type { MemberLoadState } from "@/waddles/directory";

interface ConnectionStatusClasses {
  banner: string;
  iconWrap: string;
  chip: string;
  body: string;
}

export interface ChannelHeaderMember {
  nick: string;
  jid?: string;
  /** Real bare JID disclosed for the occupant, for the avatar; `null` = initials. */
  avatarJid: string | null;
  /** Undefined for a bot, which has no status. */
  presence?: OccupantPresence;
}

const props = withDefaults(defineProps<{
  waddle: SpaceSummary | null;
  channel: ChannelSummary | null;
  dmPeer?: {
    peerJid?: string;
    peerUsername: string;
    presenceShow?: string;
    presenceIdleSince?: number;
  } | null;
  callRoomJid?: string | null;
  isForumChannel: boolean;
  canManageChannels: boolean;
  memberCount: number | null;
  memberState: MemberLoadState;
  /** Roster of room occupants with their current presence, used to render
   * the live presence avatar stack on the right of the header. Sorted
   * online-first by the caller. */
  members?: ChannelHeaderMember[];
  connectionNotice: ConnectionNoticeCopy | null;
  connectionStatusClasses: ConnectionStatusClasses | null;
  connectionStatusIcon: Component;
  /** Wired XMPP client passed to per-conversation controls (notify
   * mode picker). `null` while the session is still connecting. */
  xmppClient?: BrowserXmppClient | null;
  /** Per-controller XEP-0492 settings store — explicit wiring,
   * NOT a module singleton, per the PR-compliance note. */
  notifySettings: NotifySettingsStore;
  /** When a larger in-conversation call banner is mounted, suppress
   *  the compact header pill so users get one join affordance. */
  showCallActivePill?: boolean;
  /** Same suppression for 1:1 answer/reconnect call activity in the
   *  header; the conversation banner owns that action in ContentArea. */
  showDmCallActivityControls?: boolean;
  /** Hide channel start controls while a refreshed live-call banner
   *  presents the join affordance for this room. */
  hideMucStartControlsWhenActiveCall?: boolean;
}>(), {
  showCallActivePill: true,
  showDmCallActivityControls: true,
  hideMucStartControlsWhenActiveCall: false,
});

const MAX_HEADER_AVATARS = 4;

const sortedMembers = computed<ChannelHeaderMember[]>(() => {
  const list = props.members ?? [];
  const rank = (p?: OccupantPresence) => {
    switch (p) {
      case "online": return 0;
      case "dnd":    return 1;
      case "away":   return 2;
      default:       return 3;
    }
  };
  return [...list].sort((a, b) => {
    const r = rank(a.presence) - rank(b.presence);
    if (r !== 0) return r;
    return a.nick.localeCompare(b.nick);
  });
});

const visibleMembers = computed(() => sortedMembers.value.slice(0, MAX_HEADER_AVATARS));
const overflowCount = computed(() => Math.max(0, sortedMembers.value.length - visibleMembers.value.length));
const onlineCount = computed(() => sortedMembers.value.filter((m) => m.presence === "online").length);

const showSearch = defineModel<boolean>("showSearch", { required: true });
const showPinnedPanel = defineModel<boolean>("showPinnedPanel", { default: false });
const { activity: dmCallActivity } = useDmCallActivity(() => props.dmPeer?.peerJid);

const dmCallActivityLabel = computed(() => {
  const activity = dmCallActivity.value;
  if (!activity) return "";
  if (!hasKnownDmCallMedia(activity)) {
    if (activity.state === "accepted") return "Call active";
    if (activity.direction === "incoming") return "Incoming call";
    if (activity.direction === "outgoing") return "Calling";
    return "Call ringing";
  }
  const media = activity.media.video ? "Video" : "Voice";
  if (activity.state === "accepted") return `${media} call active`;
  if (activity.direction === "incoming") return `Incoming ${media.toLowerCase()} call`;
  if (activity.direction === "outgoing") return `Calling ${media.toLowerCase()} call`;
  return `${media} call ringing`;
});
const showDmCallActivityStatus = computed(() =>
  props.showDmCallActivityControls !== false && !!dmCallActivity.value,
);
// Live clock so the idle age refreshes as time passes. The idle tracker
// deliberately does not rebroadcast unchanged presence, so without a reactive
// `now` the computed below would never recompute and "idle 20m" would go stale.
// Ticks at minute granularity (matching formatIdle); cleared on unmount.
const nowMs = ref(Date.now());
let idleTicker: ReturnType<typeof setInterval> | null = null;
onMounted(() => {
  idleTicker = setInterval(() => {
    nowMs.value = Date.now();
  }, 60_000);
});
onBeforeUnmount(() => {
  if (idleTicker !== null) {
    clearInterval(idleTicker);
    idleTicker = null;
  }
});

// Presence line for a DM peer, appending the XEP-0319 idle age behind an
// away/xa dot ("away · idle 20m"). Recomputes both when the peer's presence
// changes and every minute as `nowMs` ticks, so the age stays current.
const dmPeerPresenceText = computed(() => {
  const show = props.dmPeer?.presenceShow;
  const base = presenceText(show);
  const idleSince = props.dmPeer?.presenceIdleSince;
  return (show === "away" || show === "xa") && idleSince !== undefined
    ? `${base} · ${formatIdle(idleSince, nowMs.value)}`
    : base;
});
// Whether the peer is in a call elsewhere (their XEP-0108 overlay, ADR-010
// Phase 3) — distinct from a call *with* us (`dmCallActivity`).
const dmPeerInCall = useInCallOverlay(() => props.dmPeer?.peerJid);
const dmHeaderSecondaryText = computed(() =>
  showDmCallActivityStatus.value
    ? dmCallActivityLabel.value
    : dmPeerInCall.value
      ? "in a call"
      : dmPeerPresenceText.value,
);

const emit = defineEmits<{
  openNav: [];
  openDetails: [];
  editChannel: [];
  selectMember: [jid: string];
}>();

// Below `lg` the header keeps only search and members inline; pins,
// notifications, channel settings and call starts move into one
// overflow menu so the conversation title keeps its width.
const dmCallStart = useDmCallStart(() => props.dmPeer?.peerJid);
const mucCallStart = useMucCallStart(() => props.callRoomJid ?? "", {
  hideWhenActiveCall: () => props.hideMucStartControlsWhenActiveCall,
});
const overflowCall = computed(() => {
  if (props.dmPeer?.peerJid) return dmCallStart;
  if (props.callRoomJid) return mucCallStart;
  return null;
});
const showOverflowCallItems = computed(() => overflowCall.value?.canStart.value ?? false);
const overflowCallBusy = computed(() => overflowCall.value?.busy.value ?? false);

function presenceLabel(p: OccupantPresence): string {
  switch (p) {
    case "online": return "online";
    case "away":   return "away";
    case "dnd":    return "do not disturb";
    default:       return "offline";
  }
}

function onAvatarClick(member: ChannelHeaderMember) {
  if (!member.jid) {
    emit("openDetails");
    return;
  }
  emit("selectMember", member.jid);
}

function presenceText(show?: string): string {
  if (show === "available") return "online";
  if (show === "away") return "away";
  if (show === "dnd") return "do not disturb";
  if (show === "xa") return "extended away";
  return "offline";
}

const dmPeerPresence = computed<OccupantPresence>(() => {
  const show = props.dmPeer?.presenceShow;
  if (show === "available" || show === "chat") return "online";
  if (show === "away" || show === "xa") return "away";
  return show === "dnd" ? "dnd" : "offline";
});

// App-Room / App-DM header: sigil or avatar, title, one muted caption, quiet icon buttons.
const rootClass = cx(header(), css({ flex: "none", gap: "2", px: "3", md: { gap: "3", px: "6" } }));
const emptyClass = css({ md: { display: "none" } });
const titleGroupClass = css({ display: "flex", alignItems: "center", gap: "2.5", minWidth: 0, flex: 1 });
const sigilClass = css({ display: "none", flex: "none", color: "fg.soft", "& svg": { width: "20px", height: "20px" }, md: { display: "inline-flex" } });
const avatarSlotClass = css({ display: "none", flex: "none", md: { display: "inline-flex" } });
const titleStackClass = css({ display: "flex", flexDirection: "column", minWidth: 0, lineHeight: 1.3, md: { flexDirection: "row", alignItems: "baseline", gap: "3" } });
const dmTitleStackClass = css({ display: "flex", flexDirection: "column", minWidth: 0, lineHeight: 1.3 });
const titleRowClass = css({ display: "flex", alignItems: "center", gap: "2", minWidth: 0 });
const titleClass = css({ margin: 0, font: "700 16px/1.3 {fonts.display}", color: "fg", truncate: true, md: { fontSize: "18px" } });
const captionClass = css({ textStyle: "caption", color: "fg.muted", truncate: true });
const callStatusClass = css({
  display: "none",
  alignItems: "center",
  gap: "1",
  maxWidth: "9rem",
  height: "22px",
  px: "2",
  flex: "none",
  borderRadius: "pill",
  bg: "live.subtle",
  color: "live.text",
  textStyle: "caption",
  fontWeight: 600,
  "& svg": { width: "12px", height: "12px", flex: "none" },
  lg: { display: "inline-flex" },
});
const actionsClass = css({ display: "flex", alignItems: "center", gap: "1", flex: "none" });
const wideOnlyClass = css({ display: "none", alignItems: "center", gap: "1", lg: { display: "flex" } });
const compactOnlyClass = css({ display: "flex", alignItems: "center", gap: "1", lg: { display: "none" } });
const navButtonClass = cx(iconbtn(), css({ ml: "-1", md: { display: "none" }, _coarse: { width: "44px", height: "44px" } }));
const connectionChipClass = css({ display: "none", md: { display: "inline-flex" } });
const stackClass = css({
  display: "inline-flex",
  alignItems: "center",
  gap: "1",
  height: "36px",
  ml: "1",
  px: "1.5",
  border: 0,
  borderRadius: "pill",
  bg: "transparent",
  color: "fg.muted",
  cursor: "pointer",
  _hover: { bg: "state.hover", color: "fg" },
});
const stackFacesClass = css({ display: "inline-flex", "& > * + *": { ml: "-6px" } });
const stackFaceClass = css({
  display: "inline-flex",
  borderRadius: "pill",
  boxShadow: "0 0 0 2px {colors.bg}",
  outline: "none",
  _focusVisible: { boxShadow: "0 0 0 2px {colors.bg}, 0 0 0 4px {colors.focus}" },
  "&[data-presence=offline]": { opacity: 0.6 },
});
const stackMoreClass = css({ textStyle: "caption", fontWeight: 600, fontVariantNumeric: "tabular-nums", px: "1" });
const stackFallbackClass = css({ display: "inline-flex", alignItems: "center", gap: "1.5", textStyle: "caption", fontWeight: 600, px: "1", "& svg": { width: "16px", height: "16px" } });

const memberButtonCopy = computed(() => {
  if (props.memberCount !== null) {
    const noun = props.memberCount === 1 ? "member" : "members";
    const suffix = props.memberState === "unavailable" ? ", last synced" : "";
    return {
      title: `${props.memberCount} ${noun}${suffix}`,
      aria: `Open details (${props.memberCount} ${noun}${suffix})`,
      primary: String(props.memberCount),
      secondary: noun,
    };
  }

  if (props.memberState === "loading") {
    return {
      title: "Members syncing",
      aria: "Open details (members syncing)",
      primary: "Syncing",
      secondary: "members",
    };
  }

  return {
    title: "Members unavailable",
    aria: "Open details (members unavailable)",
    primary: "Unavailable",
    secondary: "members",
  };
});
</script>

<template>
  <!-- With nothing open (App-DMs, Pick a room) only the phone keeps the bar, for its sidebar button. -->
  <div :class="cx(rootClass, !channel && !dmPeer && emptyClass)">
    <button
      :class="navButtonClass"
      type="button"
      aria-label="Open navigation"
      @click="emit('openNav')"
    >
      <ChevronLeft aria-hidden="true" />
    </button>
    <div v-if="channel || dmPeer" :class="titleGroupClass">
      <span v-if="dmPeer?.peerJid" :class="avatarSlotClass">
        <UserAvatar
          :name="dmPeer.peerUsername"
          :jid="conversationPeerAvatarJid(dmPeer.peerJid)"
          :presence="dmPeerPresence"
          size="md"
        />
      </span>
      <span v-else :class="sigilClass" aria-hidden="true">
        <component :is="dmPeer ? MessageCircle : isForumChannel ? MessagesSquare : Hash" :stroke-width="1.75" />
      </span>
      <div :class="dmPeer ? dmTitleStackClass : titleStackClass">
        <div :class="titleRowClass">
          <h1 :class="titleClass">
            {{ dmPeer ? dmPeer.peerUsername : channel?.name ?? "…" }}
          </h1>
          <span v-if="isForumChannel" :class="tag()">Forum</span>
          <span v-if="showDmCallActivityStatus" :class="callStatusClass">
            <PhoneCall aria-hidden="true" />
            <span :class="css({ truncate: true })">{{ dmCallActivityLabel }}</span>
          </span>
        </div>
        <span v-if="dmPeer" :class="captionClass">{{ dmHeaderSecondaryText }}</span>
        <!-- The room's own description (its topic) when it has one,
             otherwise the space name on narrow screens. -->
        <span v-else-if="channel?.description" :class="captionClass">{{ channel.description }}</span>
        <span v-else-if="channel && waddle" :class="cx(captionClass, css({ md: { display: 'none' } }))">{{ waddle.name }}</span>
      </div>
    </div>
    <div :class="actionsClass">
      <div
        v-if="connectionNotice && connectionStatusClasses"
        :class="cx(connectionChipClass, connectionStatusClasses.chip)"
      >
        <component :is="connectionStatusIcon" />
        <span>{{ connectionNotice.shortLabel }}</span>
      </div>
      <AppTooltip v-if="channel || dmPeer" label="Search messages">
        <button
          :class="iconbtn()"
          aria-label="Search messages"
          :aria-pressed="showSearch"
          type="button"
          @click="showSearch = !showSearch"
        >
          <Search aria-hidden="true" />
        </button>
      </AppTooltip>
      <!-- Wide-layout actions. Below `lg` these live in the overflow menu. -->
      <div :class="wideOnlyClass">
        <AppTooltip v-if="channel || dmPeer" label="Pinned messages">
          <button
            :class="iconbtn()"
            aria-label="Pinned messages"
            :aria-pressed="showPinnedPanel"
            type="button"
            @click="showPinnedPanel = !showPinnedPanel"
          >
            <Pin aria-hidden="true" />
          </button>
        </AppTooltip>
        <NotifyModeButton
          v-if="channel?.jid"
          :room-jid="channel.jid"
          :room-name="channel.name"
          conversation-kind="private-group"
          :client="xmppClient ?? null"
          :store="notifySettings"
        />
        <!-- Per-DM XEP-0492 notify picker (#720): the bare peer JID keys
             the same store as rooms; `direct-chat` routes the publish to
             the `urn:waddle:dm-bookmarks:0` PEP node. -->
        <NotifyModeButton
          v-else-if="dmPeer?.peerJid"
          :room-jid="dmPeer.peerJid"
          :room-name="dmPeer.peerUsername"
          conversation-kind="direct-chat"
          :client="xmppClient ?? null"
          :store="notifySettings"
        />
        <AppTooltip v-if="canManageChannels && channel" label="Room settings">
          <button
            :class="iconbtn()"
            aria-label="Channel settings"
            type="button"
            @click="emit('editChannel')"
          >
            <Settings aria-hidden="true" />
          </button>
        </AppTooltip>
        <CallButton
          v-if="dmPeer?.peerJid"
          :peer-bare-jid="dmPeer.peerJid"
          :show-activity-controls="showDmCallActivityControls !== false"
        />
        <MucCallButton
          v-else-if="callRoomJid"
          :room-jid="callRoomJid"
          :show-active-pill="showCallActivePill !== false"
          :hide-start-controls-when-active-call="hideMucStartControlsWhenActiveCall"
        />
        <!-- Who is here right now: opens the room details; a face opens that person. -->
        <AppTooltip v-if="channel" :label="memberButtonCopy.title">
          <button
            :class="stackClass"
            type="button"
            :aria-label="`${memberButtonCopy.aria}. ${onlineCount} online.`"
            @click="emit('openDetails')"
          >
            <span v-if="visibleMembers.length > 0" :class="stackFacesClass" @click.stop>
              <span
                v-for="member in visibleMembers"
                :key="`hdr-avatar:${member.nick}`"
                :class="stackFaceClass"
                :data-presence="member.presence"
                :title="member.presence ? `${member.nick} · ${presenceLabel(member.presence)}` : member.nick"
                tabindex="0"
                @click.stop="onAvatarClick(member)"
                @keydown.enter.stop="onAvatarClick(member)"
              >
                <UserAvatar
                  :name="member.nick"
                  :jid="member.avatarJid"
                  :presence="member.presence"
                  size="sm"
                />
              </span>
            </span>
            <span
              v-if="overflowCount > 0"
              :class="stackMoreClass"
              :title="`${overflowCount} more`"
            >+{{ overflowCount }}</span>
            <span v-else-if="visibleMembers.length === 0" :class="stackFallbackClass">
              <Users aria-hidden="true" />
              <span>{{ memberButtonCopy.primary }}</span>
            </span>
          </button>
        </AppTooltip>
      </div>
      <!-- Compact-layout actions: members, then everything else in one overflow menu. -->
      <div v-if="channel || dmPeer" :class="compactOnlyClass">
        <AppTooltip v-if="channel" :label="memberButtonCopy.title">
          <button
            :class="iconbtn()"
            type="button"
            :aria-label="memberButtonCopy.aria"
            @click="emit('openDetails')"
          >
            <Users aria-hidden="true" />
          </button>
        </AppTooltip>
        <ChatHeaderOverflowMenu
          :show-call-items="showOverflowCallItems"
          :call-busy="overflowCallBusy"
          :call-noun="dmPeer ? 'call' : 'huddle'"
          :pinned-open="showPinnedPanel"
          :show-channel-settings="canManageChannels && !!channel"
          @voice-call="overflowCall?.start({ audio: true, video: false })"
          @video-call="overflowCall?.start({ audio: true, video: true })"
          @toggle-pinned="showPinnedPanel = !showPinnedPanel"
          @edit-channel="emit('editChannel')"
        >
          <NotifyModeButton
            v-if="channel?.jid"
            variant="submenu"
            :room-jid="channel.jid"
            :room-name="channel.name"
            conversation-kind="private-group"
            :client="xmppClient ?? null"
            :store="notifySettings"
          />
          <NotifyModeButton
            v-else-if="dmPeer?.peerJid"
            variant="submenu"
            :room-jid="dmPeer.peerJid"
            :room-name="dmPeer.peerUsername"
            conversation-kind="direct-chat"
            :client="xmppClient ?? null"
            :store="notifySettings"
          />
        </ChatHeaderOverflowMenu>
      </div>
    </div>
  </div>
</template>
