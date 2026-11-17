<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import { Search, SearchX, X } from "lucide-vue-next";
import { css } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import UserAvatar from "@/components/ui/UserAvatar.vue";
import { formatTimelineStamp } from "@/channels/timeline";
import type { MessageSearchResult, OccupantPresence } from "@/lib/xmpp-client";

const open = defineModel<boolean>("open", { default: false });

defineProps<{
  results: MessageSearchResult[];
  isSearching: boolean;
  /** Real bare JID behind a result's author, for the avatar; `null` = initials. */
  avatarJidFor: (result: MessageSearchResult) => string | null;
  roomPresence: Record<string, OccupantPresence>;
}>();

const emit = defineEmits<{
  search: [query: string];
  clear: [];
  openResult: [result: MessageSearchResult];
}>();

const searchInput = ref("");
const searchSubmitted = ref(false);
const searchInputEl = ref<HTMLInputElement | null>(null);

// Drop the caret straight into the search input when the bar opens —
// without this the user had to click the input a second time after
// clicking the header Search button. Matches the autofocus convention
// the GIF picker already follows. Closing (from any path, including a
// conversation switch) resets the input for the next open.
watch(open, async (isOpen) => {
  if (!isOpen) {
    searchInput.value = "";
    searchSubmitted.value = false;
    return;
  }
  await nextTick();
  searchInputEl.value?.focus();
});

function doSearch() {
  searchSubmitted.value = !!searchInput.value.trim();
  emit("search", searchInput.value);
}

function closeSearch() {
  open.value = false;
  searchInput.value = "";
  searchSubmitted.value = false;
  emit("clear");
}
// One quiet search row under the header, results in a short scrolling list.
const cls = {
  bar: css({ flex: "none", display: "flex", alignItems: "center", gap: "3", px: "6", py: "2", borderBottom: "1px solid {colors.border.soft}", animation: "wd-fade {durations.fast} {easings.out}", _narrow: { px: "4" } }),
  icon: css({ width: "16px", height: "16px", flex: "none", color: "fg.muted" }),
  input: css({ flex: 1, minWidth: 0, height: "40px", border: 0, bg: "transparent", color: "fg", font: "400 15px/1.4 {fonts.body}", outline: "none", _placeholder: { color: "fg.soft" } }),
  results: css({ flex: "none", maxHeight: "224px", overflowY: "auto", borderBottom: "1px solid {colors.border.soft}", bg: "bg.rail" }),
  state: css({ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", gap: "1.5", py: "5", textStyle: "caption", color: "fg.muted", textAlign: "center" }),
  stateIcon: css({ width: "20px", height: "20px", color: "fg.soft" }),
  row: css({
    display: "flex",
    width: "100%",
    alignItems: "flex-start",
    gap: "3",
    px: "6",
    py: "3",
    border: 0,
    borderTop: "1px solid {colors.border.soft}",
    bg: "transparent",
    color: "fg",
    textAlign: "left",
    cursor: "pointer",
    _first: { borderTop: 0 },
    _hover: { bg: "state.hover" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "-3px" },
    _narrow: { px: "4" },
  }),
  stack: css({ minWidth: 0, flex: 1 }),
  meta: css({ display: "flex", alignItems: "baseline", gap: "2" }),
  nick: css({ fontSize: "sm", fontWeight: 700 }),
  time: css({ textStyle: "timestamp", color: "fg.muted", fontVariantNumeric: "tabular-nums" }),
  body: css({ margin: 0, textStyle: "caption", color: "fg.muted", truncate: true }),
};
</script>

<template>
  <!-- Search bar -->
  <div v-if="open" :class="cls.bar">
    <Search :class="cls.icon" aria-hidden="true" />
    <input
      ref="searchInputEl"
      v-model="searchInput"
      placeholder="Search messages…"
      aria-label="Search messages"
      :class="cls.input"
      @keydown.enter="doSearch"
      @keydown.escape="closeSearch"
    />
    <button
      v-if="searchInput"
      :class="iconbtn({ size: 'sm' })"
      aria-label="Clear search"
      type="button"
      @click="closeSearch"
    >
      <X aria-hidden="true" />
    </button>
  </div>

  <!-- Search results -->
  <div v-if="open && (searchSubmitted || isSearching)" :class="cls.results">
    <div v-if="isSearching" :class="cls.state" role="status">
      <span>Searching…</span>
    </div>
    <div v-else-if="results.length === 0" :class="cls.state">
      <SearchX :class="cls.stateIcon" aria-hidden="true" />
      <span>No matching messages</span>
    </div>
    <template v-else>
      <!-- Author avatar, name and time, then the matching line. -->
      <button
        v-for="result in results"
        :key="result.id"
        :class="cls.row"
        type="button"
        @click="emit('openResult', result)"
      >
        <UserAvatar
          :name="result.nick"
          :jid="avatarJidFor(result)"
          :presence="roomPresence[result.nick] ?? 'offline'"
          size="sm"
        />
        <div :class="cls.stack">
          <div :class="cls.meta">
            <span :class="cls.nick">{{ result.nick }}</span>
            <span :class="cls.time">{{ formatTimelineStamp(result.createdAt) }}</span>
          </div>
          <p :class="cls.body">{{ result.body }}</p>
        </div>
      </button>
    </template>
  </div>
</template>
