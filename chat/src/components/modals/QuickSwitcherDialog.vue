<script setup lang="ts">
import { computed, nextTick, ref, useId, watch, type Component } from "vue";
import {
  CalendarDays,
  Hash,
  Home,
  Inbox,
  LayoutGrid,
  ListTree,
  MessageSquareText,
  MessagesSquare,
  Search,
  Settings,
  Users,
} from "lucide-vue-next";
import { css, cx } from "styled-system/css";
import { badge } from "styled-system/recipes";
import AppDialog from "@/components/ui/AppDialog.vue";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { buildHomeChannelUnreadMap } from "@/home/dashboard-props";
import {
  buildQuickSwitcherEntries,
  moveQuickSwitcherHighlight,
  rankQuickSwitcherEntries,
  resolveQuickSwitcherHighlight,
  type QuickSwitcherEntry,
  type QuickSwitcherPage,
} from "@/lib/quick-switcher";
import type { ChatAppController } from "@/shell/chat-app-controller";

/**
 * ⌘K / Ctrl+K: type to filter rooms, people and pages, ↑/↓ to move,
 * Enter to open, Escape to close. The shortcut lives in useChatKeyboard.
 */
const props = defineProps<{
  controller: ChatAppController;
}>();

const {
  ui,
  waddles,
  messaging,
  computedChannelUnreadMap,
  groupDmConversations,
  dmConversations,
  rosterContacts,
  selectChannel,
  selectGroupDm,
  selectDm,
  openHome,
  openRooms,
  openThreads,
  openUnread,
  openMembers,
  openCommunitySurface,
  openUserSettings,
} = props.controller;

const PAGES: Record<QuickSwitcherPage, { icon: Component; open: () => void }> = {
  home: { icon: Home, open: openHome },
  rooms: { icon: LayoutGrid, open: openRooms },
  threads: { icon: ListTree, open: openThreads },
  unread: { icon: Inbox, open: openUnread },
  members: { icon: Users, open: openMembers },
  feed: { icon: MessageSquareText, open: () => openCommunitySurface("feed") },
  events: { icon: CalendarDays, open: () => openCommunitySurface("events") },
  settings: { icon: Settings, open: openUserSettings },
};

const open = ui.showQuickSwitcher;
const query = ref("");
const highlighted = ref<string | null>(null);
const listEl = ref<HTMLElement | null>(null);
const titleId = useId();
const listId = useId();

const entries = computed(() => {
  const channels = waddles.sortedChannels.value;
  return buildQuickSwitcherEntries({
    spaces: waddles.sortedSpaces.value,
    channels,
    channelUnread: buildHomeChannelUnreadMap(
      channels,
      computedChannelUnreadMap.value,
      messaging.mentionedChannelCounts.value,
    ),
    groupDms: groupDmConversations.value,
    conversations: dmConversations.conversations.value,
    contacts: rosterContacts.contacts.value,
  });
});
const results = computed(() => rankQuickSwitcherEntries(entries.value, query.value));
const resultIds = computed(() => results.value.map((entry) => entry.id));
const current = computed(() => resolveQuickSwitcherHighlight(highlighted.value, resultIds.value));
const currentIndex = computed(() => (current.value === null ? -1 : resultIds.value.indexOf(current.value)));

watch(open, (isOpen) => {
  if (!isOpen) return;
  query.value = "";
  highlighted.value = null;
});

watch(query, () => {
  highlighted.value = null;
});

function optionId(index: number): string {
  return `${listId}-option-${index}`;
}

function move(offset: number) {
  highlighted.value = moveQuickSwitcherHighlight(current.value, offset, resultIds.value);
  void nextTick(() => {
    listEl.value?.querySelector("[aria-selected='true']")?.scrollIntoView({ block: "nearest" });
  });
}

function onKeydown(event: KeyboardEvent) {
  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    event.preventDefault();
    move(event.key === "ArrowDown" ? 1 : -1);
    return;
  }
  if (event.key !== "Enter" || event.isComposing || Reflect.get(event, "keyCode") === 229) return;
  event.preventDefault();
  const entry = results.value[currentIndex.value];
  if (entry) choose(entry);
}

function choose(entry: QuickSwitcherEntry) {
  open.value = false;
  const { target } = entry;
  switch (target.kind) {
    case "channel":
      void selectChannel(target.channelId, target.roomJid ? { roomJid: target.roomJid } : undefined);
      return;
    case "groupDm":
      void selectGroupDm(target.roomJid);
      return;
    case "dm":
      void selectDm(target.peerJid);
      return;
    case "page":
      PAGES[target.page].open();
  }
}

function entryIcon(entry: QuickSwitcherEntry): Component {
  switch (entry.target.kind) {
    case "page":
      return PAGES[entry.target.page].icon;
    case "channel":
      return entry.forum ? MessagesSquare : Hash;
    default:
      return MessagesSquare;
  }
}

const GROUP_LABEL: Record<QuickSwitcherEntry["target"]["kind"], string> = {
  channel: "Rooms",
  groupDm: "Group chats",
  dm: "People",
  page: "Pages",
};
/** Shell-Jump board: a group heading wherever the ranked list changes kind. */
function groupHeading(index: number): string | null {
  const kind = results.value[index]?.target.kind;
  if (!kind || results.value[index - 1]?.target.kind === kind) return null;
  return GROUP_LABEL[kind];
}

const srOnly = css({ srOnly: true });
const searchRow = css({ display: "flex", alignItems: "center", gap: "2.5", py: "4", px: "4", borderBottom: "1px solid {colors.border.soft}", "& > svg": { width: "20px", height: "20px", flex: "none", color: "fg.muted" } });
const inputClass = css({ flex: 1, minWidth: 0, border: 0, bg: "transparent", color: "fg", font: "18px/1.4 {fonts.body}", outline: "none", _placeholder: { color: "fg.soft" }, _coarse: { fontSize: "base" } });
const kbd = css({ font: "600 12px/1 {fonts.mono}", py: "1", px: "2", borderRadius: "tag", border: "1px solid {colors.border}", color: "fg.muted" });
const list = css({ minHeight: 0, flex: 1, overflowY: "auto", margin: 0, listStyle: "none", pt: "1.5", px: "2", pb: "2.5" });
const groupLabel = css({ margin: 0, pt: "2.5", px: "3", pb: "1", font: "700 13px/1.3 {fonts.body}", color: "fg.muted" });
const optionClass = css({
  display: "flex",
  alignItems: "center",
  gap: "3",
  minHeight: "44px",
  px: "3",
  borderRadius: "control",
  fontSize: "md",
  color: "fg",
  cursor: "pointer",
  "&[aria-selected=true]": { bg: "action.subtle" },
});
const iconClass = css({ display: "inline-flex", width: "28px", height: "28px", flex: "none", alignItems: "center", justifyContent: "center", color: "fg.muted", "& svg": { width: "18px", height: "18px" } });
const textClass = css({ minWidth: 0, flex: 1, display: "flex", flexDirection: "column" });
const titleClass = css({ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const hintClass = css({ fontSize: "xs", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });
const unreadBadge = badge({ variant: "quiet" });
const mentionBadge = badge();
const empty = css({ margin: 0, py: "6", px: "4", textAlign: "center", fontSize: "sm", color: "fg.muted" });
const footer = cx(css({ display: "flex", gap: "4", py: "2.5", px: "4", borderTop: "1px solid {colors.border.soft}", fontSize: "xs", color: "fg.muted" }), css({ _coarse: { display: "none" } }));

function badgeLabel(entry: QuickSwitcherEntry): string {
  const unread = entry.unread > 0 ? `${entry.unread} unread` : "";
  return entry.mentionsMe ? [unread, "mentions you"].filter(Boolean).join(", ") : unread;
}
</script>

<template>
  <AppDialog v-model:open="open" :labelled-by="titleId" size="lg">
    <h2 :id="titleId" :class="srOnly">Jump to a room, person or page</h2>
    <div :class="searchRow">
      <Search aria-hidden="true" />
      <input
        v-model="query"
        type="text"
        role="combobox"
        aria-label="Jump to"
        aria-autocomplete="list"
        :aria-expanded="results.length > 0"
        :aria-controls="results.length > 0 ? listId : undefined"
        :aria-activedescendant="currentIndex >= 0 ? optionId(currentIndex) : undefined"
        placeholder="Search or jump to…"
        autocomplete="off"
        spellcheck="false"
        :class="inputClass"
        @keydown="onKeydown"
      />
      <kbd :class="kbd" aria-hidden="true">Esc</kbd>
    </div>
    <ul
      v-if="results.length > 0"
      :id="listId"
      ref="listEl"
      role="listbox"
      aria-label="Results"
      :class="list"
    >
      <template v-for="(entry, index) in results" :key="entry.id">
        <li v-if="groupHeading(index)" role="presentation" :class="groupLabel" aria-hidden="true">{{ groupHeading(index) }}</li>
        <li
          :id="optionId(index)"
          role="option"
          :aria-selected="index === currentIndex"
          :class="optionClass"
          @mousemove="highlighted = entry.id"
          @mousedown.prevent
          @click="choose(entry)"
          @keydown.enter.prevent="choose(entry)"
        >
          <UserAvatar v-if="entry.target.kind === 'dm'" :jid="entry.target.peerJid" :name="entry.title" size="sm" />
          <span v-else :class="iconClass">
            <component :is="entryIcon(entry)" aria-hidden="true" />
          </span>
          <span :class="textClass">
            <span :class="titleClass">{{ entry.title }}</span>
            <span v-if="entry.subtitle" :class="hintClass">{{ entry.subtitle }}</span>
          </span>
          <template v-if="entry.unread > 0 || entry.mentionsMe">
            <span :class="entry.mentionsMe ? mentionBadge : unreadBadge" aria-hidden="true">{{ entry.unread > 0 ? entry.unread : "@" }}</span>
            <span :class="srOnly">{{ badgeLabel(entry) }}</span>
          </template>
        </li>
      </template>
    </ul>
    <p v-else role="status" :class="empty">No matches</p>
    <div :class="footer" aria-hidden="true"><span>↑ ↓ to move</span><span>Enter to open</span></div>
  </AppDialog>
</template>
