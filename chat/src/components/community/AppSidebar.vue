<script setup lang="ts">
import { computed } from "vue";
import {
  CalendarDays,
  ChevronDown,
  Compass,
  Hash,
  House,
  Inbox,
  MessagesSquare,
  Search,
  SquarePen,
  Users,
} from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { badge, iconbtn, room } from "styled-system/recipes";
import ProfilePanel from "@/components/chat/ProfilePanel.vue";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { buildHomeChannelUnreadMap } from "@/home/dashboard-props";
import { conversationPeerAvatarJid } from "@/lib/avatars/author-jid";
import { isForumChannel } from "@/lib/channel-types";
import type { OccupantPresence } from "@/lib/xmpp-client";
import { buildHref, type RouteMatch } from "@/router";
import type { ChatAppController } from "@/shell/chat-app-controller";

/**
 * The shell sidebar (Sidebar board): community, ⌘K jump, your views,
 * what is live (`#live` slot), rooms, direct messages, the huddle you are
 * in (`#dock` slot) and your account. Ink in both themes: it renders
 * `data-theme="night"` (except as the phone home screen). Every action is an existing controller
 * call; the sidebar holds no state of its own.
 */
const props = defineProps<{
  controller: ChatAppController;
  /** The phone home screen (Mobile-Home board) follows the page theme. */
  followTheme?: boolean;
}>();

const {
  ui,
  waddles,
  messaging,
  connectionStore,
  channelUnread,
  computedChannelUnreadMap,
  groupDmConversations,
  dmConversations,
  notifications,
  version,
  openHome,
  openUnread,
  openMembers,
  openRooms,
  openCommunitySurface,
  openUserSettings,
  selectChannel,
  selectGroupDm,
  selectDm,
  handleLogout,
  handleRequestNotifications,
  handleToggleNotifications,
  handleToggleMessageSounds,
} = props.controller;

type ViewId = "home" | "inbox" | "calendar" | "people";

const views: { id: ViewId; label: string; icon: typeof House; href: string; go: () => void }[] = [
  { id: "home", label: "Home", icon: House, href: buildHref({ id: "home" } as RouteMatch), go: () => openHome() },
  { id: "inbox", label: "Inbox", icon: Inbox, href: buildHref({ id: "unread" } as RouteMatch), go: () => openUnread() },
  { id: "calendar", label: "Calendar", icon: CalendarDays, href: buildHref({ id: "events" } as RouteMatch), go: () => openCommunitySurface("events") },
  { id: "people", label: "People", icon: Users, href: buildHref({ id: "members" } as RouteMatch), go: () => openMembers() },
];

const activeView = computed<ViewId | null>(() => {
  if (ui.activeCommunitySurface.value === "events") return "calendar";
  if (ui.activeCommunitySurface.value === "feed") return null;
  switch (ui.activePage.value) {
    case "dashboard": return "home";
    case "unread":
    case "threads": return "inbox";
    case "members": return "people";
    default: return null;
  }
});

function viewCount(id: ViewId): number {
  // Inbox holds Unread and Discussions, so its count covers thread replies too.
  if (id === "inbox") return channelUnread.totalMentionCount.value || channelUnread.totalUnreadCount.value + channelUnread.totalThreadUnreadCount.value;
  return 0;
}

const communityName = computed(() =>
  waddles.currentSpace.value?.name ?? waddles.sortedSpaces.value[0]?.name ?? "Waddle",
);

const inConversation = computed(() => ui.activePage.value === "chat" && ui.activeCommunitySurface.value === null);

const rooms = computed(() => {
  const channels = waddles.sortedChannels.value.filter((channel) => !channel.isGroupDm);
  const unread = buildHomeChannelUnreadMap(channels, computedChannelUnreadMap.value, messaging.mentionedChannelCounts.value);
  return channels.map((channel) => {
    const activity = unread[channel.id] ?? { unread: 0, mentions: 0 };
    return {
      channel,
      forum: isForumChannel(channel),
      unread: activity.unread,
      mentions: activity.mentions,
      current: inConversation.value && ui.sidebarMode.value === "channels" && waddles.activeChannelId.value === channel.id,
    };
  });
});

const groupDms = computed(() => groupDmConversations.value.map((group) => ({
  group,
  current: inConversation.value && waddles.activeChannelId.value === group.id,
})));

const dms = computed(() => dmConversations.conversations.value.map((conversation) => ({
  conversation,
  current: inConversation.value && ui.sidebarMode.value === "dms" && dmConversations.activePeerJid.value === conversation.peerJid,
})));

function presenceOf(show?: string): OccupantPresence | undefined {
  switch (show) {
    case "available": return "online";
    case "away":
    case "xa": return "away";
    case "dnd": return "dnd";
    case "offline": return "offline";
    default: return undefined;
  }
}

function go(action: () => void) {
  ui.showMobileNav.value = false;
  action();
}

const rootClass = css({
  "--ring-bg": "{colors.bg.rail}",
  display: "flex",
  flexDirection: "column",
  width: "100%",
  height: "100%",
  minHeight: 0,
  bg: "bg.rail",
  color: "fg",
  fontFamily: "body",
  overflow: "hidden",
});
const topClass = css({ display: "flex", alignItems: "center", gap: "1.5", padding: "12px 10px 8px 12px", flex: "none" });
const communityClass = css({
  flex: 1,
  minWidth: 0,
  display: "flex",
  alignItems: "center",
  gap: "2.5",
  height: "44px",
  padding: "0 8px 0 2px",
  border: 0,
  borderRadius: "control",
  bg: "transparent",
  color: "fg",
  textAlign: "left",
  cursor: "pointer",
  _hover: { bg: "state.hover" },
  "& > svg": { width: "16px", height: "16px", flex: "none", color: "fg.muted" },
});
const markClass = css({
  display: "inline-flex",
  flex: "none",
  width: "36px",
  height: "36px",
  borderRadius: "control",
  bg: "belly",
  alignItems: "center",
  justifyContent: "center",
  "& img": { width: "26px", height: "26px" },
});
const communityNameClass = css({ flex: 1, minWidth: 0, font: "800 17px/1.2 {fonts.display}", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" });
const jumpWrapClass = css({ padding: "0 10px 6px", flex: "none" });
const jumpClass = css({
  width: "100%",
  display: "flex",
  alignItems: "center",
  gap: "2",
  height: "40px",
  padding: "0 10px",
  borderRadius: "control",
  border: "1px solid {colors.border.strong}",
  bg: "surface",
  color: "fg.soft",
  font: "400 14px/1 {fonts.body}",
  cursor: "pointer",
  _hover: { borderColor: "fg.soft" },
  "& svg": { width: "18px", height: "18px", flex: "none" },
  "& span": { flex: 1, textAlign: "left" },
  "& kbd": { font: "600 12px/1 {fonts.mono}", padding: "4px 6px", borderRadius: "tag", border: "1px solid {colors.border}", color: "fg.muted", _coarse: { display: "none" } },
});
const scrollClass = css({ flex: 1, minHeight: 0, overflowY: "auto", padding: "0 8px 8px", display: "flex", flexDirection: "column", gap: "0.5" });
const rowClass = cx(room(), css({ cursor: "pointer", border: 0, bg: "transparent", width: "100%", textAlign: "left", "& .wd-badge": { flex: "none" } }));
const viewRowClass = cx(rowClass, css({ color: "fg" }));
const sectionClass = css({
  display: "flex",
  alignItems: "center",
  gap: "1",
  padding: "14px 8px 4px 12px",
  "& h2": { margin: 0, flex: 1, font: "700 13px/1.3 {fonts.body}", color: "fg.muted" },
  "& .wd-iconbtn": { width: "32px", height: "32px", "& svg": { width: "18px", height: "18px" } },
});
const emptyClass = css({ margin: "2px 12px 6px", fontSize: "sm", lineHeight: "1.45", color: "fg.muted" });
const footClass = css({ display: "flex", alignItems: "center", gap: "2.5", padding: "10px 10px 12px 14px", marginTop: "1.5", borderTop: "1px solid {colors.border.soft}", flex: "none" });
const dockClass = css({ flex: "none", padding: "6px 10px 0", display: "flex", flexDirection: "column", gap: "2", _empty: { display: "none" } });
</script>

<template>
  <nav :class="rootClass" :aria-label="communityName" :data-theme="followTheme ? undefined : 'night'">
    <div :class="topClass">
      <button
        type="button"
        :class="communityClass"
        :aria-label="`${communityName}, go to Home`"
        @click="go(openHome)"
      >
        <span :class="markClass" aria-hidden="true"><img src="/waddle-logo.svg" alt="" /></span>
        <span :class="communityNameClass">{{ communityName }}</span>
        <ChevronDown aria-hidden="true" />
      </button>
      <button type="button" :class="iconbtn()" aria-label="New message" @click="ui.showNewDm.value = true">
        <SquarePen aria-hidden="true" />
      </button>
    </div>
    <div :class="jumpWrapClass">
      <button
        type="button"
        :class="jumpClass"
        aria-label="Search or jump to, Command K"
        @click="ui.showMobileNav.value = false; ui.showQuickSwitcher.value = true"
      >
        <Search aria-hidden="true" />
        <span>Search or jump to…</span>
        <kbd aria-hidden="true">⌘K</kbd>
      </button>
    </div>

    <div :class="scrollClass">
      <a
        v-for="view in views"
        :key="view.id"
        :href="view.href"
        :class="viewRowClass"
        :aria-current="activeView === view.id ? 'page' : undefined"
        @click.prevent="go(view.go)"
      >
        <component :is="view.icon" aria-hidden="true" />
        <span class="wd-room__name">{{ view.label }}</span>
        <span v-if="viewCount(view.id) > 0" :class="badge()" :aria-label="`${viewCount(view.id)} unread`">{{ viewCount(view.id) }}</span>
      </a>

      <slot name="live" />

      <div :class="sectionClass">
        <h2>Rooms</h2>
        <a
          :href="buildHref({ id: 'rooms' } as RouteMatch)"
          :class="iconbtn()"
          aria-label="Browse all rooms"
          :aria-current="ui.activePage.value === 'rooms' ? 'page' : undefined"
          @click.prevent="go(openRooms)"
        >
          <Compass aria-hidden="true" />
        </a>
      </div>
      <p v-if="rooms.length === 0" :class="emptyClass">No rooms yet. Browse to create the first one.</p>
      <button
        v-for="entry in rooms"
        :key="entry.channel.id"
        type="button"
        :class="rowClass"
        :aria-current="entry.current ? 'page' : undefined"
        :data-unread="entry.unread > 0 || entry.mentions > 0 ? '' : undefined"
        @click="go(() => selectChannel(entry.channel.id))"
      >
        <component :is="entry.forum ? MessagesSquare : Hash" aria-hidden="true" />
        <span class="wd-room__name">{{ entry.channel.name }}</span>
        <span v-if="entry.mentions > 0" :class="badge()" :aria-label="`${entry.mentions} mention${entry.mentions === 1 ? '' : 's'}`">{{ entry.mentions }}</span>
        <span v-else-if="entry.unread > 0" :class="badge({ variant: 'quiet' })" :aria-label="`${entry.unread} unread`">{{ entry.unread }}</span>
      </button>

      <div :class="sectionClass"><h2>Direct messages</h2></div>
      <p v-if="dms.length === 0 && groupDms.length === 0" :class="emptyClass">No conversations yet.</p>
      <button
        v-for="entry in groupDms"
        :key="entry.group.id"
        type="button"
        :class="rowClass"
        :aria-current="entry.current ? 'page' : undefined"
        :data-unread="entry.group.unreadCount > 0 ? '' : undefined"
        @click="go(() => selectGroupDm(entry.group.roomJid))"
      >
        <Users aria-hidden="true" />
        <span class="wd-room__name">{{ entry.group.name }}</span>
        <span v-if="entry.group.unreadCount > 0" :class="badge()" :aria-label="`${entry.group.unreadCount} unread`">{{ entry.group.unreadCount }}</span>
      </button>
      <button
        v-for="entry in dms"
        :key="entry.conversation.peerJid"
        type="button"
        :class="rowClass"
        :aria-current="entry.current ? 'page' : undefined"
        :data-unread="entry.conversation.unreadCount > 0 ? '' : undefined"
        @click="go(() => selectDm(entry.conversation.peerJid))"
      >
        <UserAvatar
          :name="entry.conversation.peerUsername"
          :jid="conversationPeerAvatarJid(entry.conversation.peerJid)"
          :presence="presenceOf(entry.conversation.presenceShow)"
          size="sm"
        />
        <span class="wd-room__name">{{ entry.conversation.peerUsername }}</span>
        <span v-if="entry.conversation.unreadCount > 0" :class="badge()" :aria-label="`${entry.conversation.unreadCount} unread`">{{ entry.conversation.unreadCount }}</span>
      </button>
    </div>

    <div :class="dockClass"><slot name="dock" /></div>

    <div v-if="connectionStore.session" :class="footClass">
      <ProfilePanel
        :session="connectionStore.session"
        :notification-permission="notifications.permissionState.value"
        :notifications-enabled="notifications.notificationsEnabled.value"
        :message-sounds-enabled="notifications.messageSoundsEnabled.value"
        :total-unread-count="channelUnread.totalUnreadCount.value"
        :total-mention-count="channelUnread.totalMentionCount.value"
        :web-commit-sha="version.webCommitSha.value"
        :server-version="version.serverVersion.value"
        placement="top-start"
        @open-settings="go(openUserSettings)"
        @logout="handleLogout"
        @request-notifications="handleRequestNotifications"
        @toggle-notifications="handleToggleNotifications"
        @toggle-message-sounds="handleToggleMessageSounds"
      />
    </div>
  </nav>
</template>
