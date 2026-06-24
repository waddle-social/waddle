<script setup lang="ts">
import { computed, type Component } from "vue";
import { Hash, Menu, MessageCircle, MessagesSquare, PhoneCall, Pin, Search, Settings, Users } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import CallButton from "@/components/calls/CallButton.vue";
import MucCallButton from "@/components/calls/MucCallButton.vue";
import NotifyModeButton from "@/components/chat/NotifyModeButton.vue";
import { hasKnownDmCallMedia, useDmCallActivity } from "@/lib/calls/dm-call-activity";
import { formatIdle } from "@/presence/idle-duration";
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
  avatarUrl?: string | null;
  presence: OccupantPresence;
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
  const rank = (p: OccupantPresence) => {
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
// Presence line for a DM peer, appending the XEP-0319 idle age behind an
// away/xa dot ("away · idle 20m"). Recomputed when the peer's presence/idle
// changes; it does not tick live, which is fine at minute granularity.
const dmPeerPresenceText = computed(() => {
  const show = props.dmPeer?.presenceShow;
  const base = presenceText(show);
  const idleSince = props.dmPeer?.presenceIdleSince;
  return (show === "away" || show === "xa") && idleSince !== undefined
    ? `${base} · ${formatIdle(idleSince, Date.now())}`
    : base;
});
const dmHeaderSecondaryText = computed(() =>
  showDmCallActivityStatus.value ? dmCallActivityLabel.value : dmPeerPresenceText.value,
);

const emit = defineEmits<{
  openNav: [];
  openDetails: [];
  editChannel: [];
  selectMember: [jid: string];
}>();

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
  <div class="chat-pane-header border-b border-border px-[var(--chat-content-inline)] py-0 flex flex-shrink-0 items-center justify-between gap-[var(--space-md)] glass-surface">
    <div class="chat-message-lane flex min-w-0 items-center gap-2">
      <button
        class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground lg:hidden"
        type="button"
        aria-label="Open navigation"
        @click="emit('openNav')"
      >
        <Menu class="w-4 h-4" />
      </button>
      <div class="chat-pane-title-group">
        <span class="chat-pane-title-icon rounded-lg bg-primary/8">
          <component :is="dmPeer ? MessageCircle : isForumChannel ? MessagesSquare : Hash" class="w-4 h-4 text-primary/70" />
        </span>
        <div class="min-w-0">
          <div class="flex min-w-0 items-center gap-2">
            <h1 class="type-chat-title truncate">
              {{ dmPeer ? dmPeer.peerUsername : channel?.name ?? "…" }}
            </h1>
            <span
              v-if="isForumChannel"
              class="type-badge rounded-full border border-primary/15 bg-primary/8 px-2 py-0.5 text-primary/80"
            >
              Forum
            </span>
            <span v-if="dmPeer" class="hidden lg:inline type-meta text-muted-foreground">
              · {{ dmPeerPresenceText }}
            </span>
            <span
              v-if="showDmCallActivityStatus"
              class="type-meta hidden h-5 max-w-[9rem] shrink-0 items-center gap-1 overflow-hidden rounded-full border border-success/25 bg-success/10 px-2 text-success lg:inline-flex"
            >
              <PhoneCall class="h-3 w-3" />
              <span class="truncate">{{ dmCallActivityLabel }}</span>
            </span>
          </div>
          <div v-if="dmPeer" class="lg:hidden type-caption text-muted-foreground truncate">
            {{ dmHeaderSecondaryText }}
          </div>
          <!-- Desktop subtitle slot: prefer the channel's own description
               (its "topic") so rooms with a stated subject get character
               at the top of the chat instead of an empty title bar.
               Falls back to the space name (already what mobile shows)
               when no description is set. Single-line, truncate, muted
               so it sits behind the H1 title without competing. -->
          <div v-else-if="channel?.description" class="type-caption text-muted-foreground truncate">
            {{ channel.description }}
          </div>
          <div v-else-if="channel && waddle" class="lg:hidden type-caption text-muted-foreground truncate">
            {{ waddle.name }}
          </div>
        </div>
      </div>
    </div>
    <div class="flex shrink-0 items-center gap-3">
      <div
        v-if="connectionNotice && connectionStatusClasses"
        class="type-caption type-emphasis hidden md:inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1"
        :class="connectionStatusClasses.chip"
      >
        <component
          :is="connectionStatusIcon"
          class="w-3.5 h-3.5"
          :class="{ 'motion-safe:animate-spin': connectionNotice.tone === 'reconnecting' }"
        />
        <span>{{ connectionNotice.shortLabel }}</span>
      </div>
      <div class="flex items-center gap-1.5">
        <!-- Header toggle buttons (Search, Pin) — the active state
             reads as "armed" instead of merely "hovered" by swapping
             the bg-muted treatment for a primary-tinted fill + a
             small primary ring. Hover stays bg-muted so the three
             visual states (rest / hover / armed) separate cleanly. -->
        <button
          v-if="channel || dmPeer"
          class="chat-icon-button chat-icon-button--md transition-all duration-200"
          :class="showSearch
            ? 'bg-muted text-primary ring-1 ring-primary/40 shadow-[0_0_10px_var(--glow)]'
            : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
          title="Search messages"
          aria-label="Search messages"
          :aria-pressed="showSearch"
          type="button"
          @click="showSearch = !showSearch"
        >
          <Search class="w-3.5 h-3.5" />
        </button>
        <button
          v-if="channel || dmPeer"
          class="chat-icon-button chat-icon-button--md transition-all duration-200"
          :class="showPinnedPanel
            ? 'bg-muted text-primary ring-1 ring-primary/40 shadow-[0_0_10px_var(--glow)]'
            : 'text-muted-foreground hover:bg-muted hover:text-foreground'"
          title="Pinned messages"
          aria-label="Pinned messages"
          :aria-pressed="showPinnedPanel"
          type="button"
          @click="showPinnedPanel = !showPinnedPanel"
        >
          <Pin class="w-3.5 h-3.5" />
        </button>
        <NotifyModeButton
          v-if="channel?.jid"
          :room-jid="channel.jid"
          :room-name="channel.name"
          conversation-kind="private-group"
          :client="xmppClient ?? null"
          :store="notifySettings"
        />
        <!-- Per-DM XEP-0492 notify picker (#720). The DM's bare peer
             JID keys the same unified store as channels; the
             `direct-chat` kind routes the publish to the
             `urn:waddle:dm-bookmarks:0` PEP node and resolves the §3
             default (`always`). The rich-payload toggle (#719) is the
             same control channels expose. -->
        <NotifyModeButton
          v-else-if="dmPeer?.peerJid"
          :room-jid="dmPeer.peerJid"
          :room-name="dmPeer.peerUsername"
          conversation-kind="direct-chat"
          :client="xmppClient ?? null"
          :store="notifySettings"
        />
        <button
          v-if="canManageChannels && channel"
          class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground"
          title="Channel settings"
          aria-label="Channel settings"
          type="button"
          @click="emit('editChannel')"
        >
          <Settings class="w-3.5 h-3.5" />
        </button>
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
        <!-- Live presence stack — desktop only. The mobile header is
             already crowded with hamburger + channel chip + search/pin/
             settings + the user's own profile avatar at the very right;
             dropping more avatars into that lane reads as "the app
             changed my profile picture" rather than "these other
             people are here." Mobile gets the compact count button
             instead. -->
        <button
          v-if="channel"
          class="chat-presence-stack hidden lg:inline-flex"
          type="button"
          :title="memberButtonCopy.title"
          :aria-label="`${memberButtonCopy.aria}. ${onlineCount} online.`"
          @click="emit('openDetails')"
        >
          <span
            v-if="visibleMembers.length > 0"
            class="chat-presence-stack__avatars"
            @click.stop
          >
            <span
              v-for="member in visibleMembers"
              :key="`hdr-avatar:${member.nick}`"
              class="chat-presence-stack__avatar-wrap"
              :class="`chat-presence-stack__avatar-wrap--${member.presence}`"
              :title="`${member.nick} · ${presenceLabel(member.presence)}`"
              tabindex="0"
              @click.stop="onAvatarClick(member)"
              @keydown.enter.stop="onAvatarClick(member)"
            >
              <AppAvatar
                :name="member.nick"
                :src="member.avatarUrl ?? null"
                :presence="member.presence"
                size="xs"
              />
            </span>
          </span>
          <span
            v-if="overflowCount > 0"
            class="chat-presence-stack__overflow"
            :title="`${overflowCount} more`"
          >+{{ overflowCount }}</span>
          <span v-else-if="visibleMembers.length === 0" class="chat-presence-stack__fallback">
            <Users class="w-3.5 h-3.5" />
            <span class="type-control">{{ memberButtonCopy.primary }}</span>
          </span>
        </button>
        <!-- Mobile members button — same data, no avatars. Keeps the
             tap-target obvious and prevents the corner avatar from
             reading as the user's own profile picture. -->
        <button
          v-if="channel"
          class="chat-icon-button chat-icon-button--md lg:hidden text-muted-foreground hover:bg-muted hover:text-foreground"
          type="button"
          :title="memberButtonCopy.title"
          :aria-label="memberButtonCopy.aria"
          @click="emit('openDetails')"
        >
          <Users class="w-4 h-4" />
        </button>
      </div>
    </div>
  </div>
</template>
