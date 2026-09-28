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

function badgeLabel(entry: QuickSwitcherEntry): string {
  const unread = entry.unread > 0 ? `${entry.unread} unread` : "";
  return entry.mentionsMe ? [unread, "mentions you"].filter(Boolean).join(", ") : unread;
}
</script>

<template>
  <AppDialog v-model:open="open" :labelled-by="titleId">
    <h2 :id="titleId" class="sr-only">Jump to a room, person or page</h2>
    <div class="border-b border-border p-2">
      <div class="relative min-w-0 rounded-lg bg-muted/60 transition-colors focus-within:bg-muted/75 focus-within:ring-1 focus-within:ring-inset focus-within:ring-primary/20">
        <Search
          class="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground"
          aria-hidden="true"
        />
        <input
          v-model="query"
          type="text"
          role="combobox"
          aria-label="Jump to"
          aria-autocomplete="list"
          :aria-expanded="results.length > 0"
          :aria-controls="results.length > 0 ? listId : undefined"
          :aria-activedescendant="currentIndex >= 0 ? optionId(currentIndex) : undefined"
          placeholder="Jump to a room, person or page"
          autocomplete="off"
          spellcheck="false"
          class="type-field min-h-11 w-full rounded-lg border-none bg-transparent py-2.5 pl-10 pr-3 placeholder:text-muted-foreground/40 focus:outline-none"
          @keydown="onKeydown"
        />
      </div>
    </div>
    <ul
      v-if="results.length > 0"
      :id="listId"
      ref="listEl"
      role="listbox"
      aria-label="Results"
      class="min-h-0 flex-1 overflow-y-auto p-1"
    >
      <li
        v-for="(entry, index) in results"
        :id="optionId(index)"
        :key="entry.id"
        role="option"
        :aria-selected="index === currentIndex"
        class="flex min-h-11 cursor-pointer items-center gap-3 rounded-lg px-3 py-1.5 transition-colors"
        :class="index === currentIndex ? 'bg-primary/15' : 'hover:bg-muted'"
        @mousemove="highlighted = entry.id"
        @mousedown.prevent
        @click="choose(entry)"
      >
        <UserAvatar v-if="entry.target.kind === 'dm'" :jid="entry.target.peerJid" :name="entry.title" size="sm" />
        <span
          v-else
          class="flex h-7 w-7 flex-shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground"
        >
          <component :is="entryIcon(entry)" class="h-4 w-4" aria-hidden="true" />
        </span>
        <span class="min-w-0 flex-1">
          <span class="type-control block truncate">{{ entry.title }}</span>
          <span v-if="entry.subtitle" class="type-caption block truncate text-muted-foreground">{{ entry.subtitle }}</span>
        </span>
        <template v-if="entry.unread > 0 || entry.mentionsMe">
          <span
            class="type-count-badge inline-flex h-[18px] min-w-[18px] items-center justify-center rounded-full px-1"
            :class="entry.mentionsMe ? 'bg-live text-live-foreground' : 'bg-primary text-primary-foreground'"
            aria-hidden="true"
          >{{ entry.unread > 0 ? entry.unread : "@" }}</span>
          <span class="sr-only">{{ badgeLabel(entry) }}</span>
        </template>
      </li>
    </ul>
    <p v-else role="status" class="type-caption px-4 py-6 text-center text-muted-foreground">No matches</p>
  </AppDialog>
</template>
