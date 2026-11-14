<script setup lang="ts">
import { computed, onMounted } from "vue";
import { Search } from "lucide-vue-next";
import spotEmpty from "@waddle/design/assets/spot-empty.svg?url";
import { css, cx } from "styled-system/css";
import { btn, input, seg, waddle } from "styled-system/recipes";
import { pageClasses as pc } from "@/components/chat/inbox-classes";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ChannelSummary } from "@/lib/chat-types";
import type { CallMedia } from "@/lib/calls/types";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import {
  type ThreadsSort,
  type ThreadsStatusFilter,
} from "@/lib/threads-view-filters";
import { useThreadsListPanelState } from "@/lib/threads-view-state";
import ThreadsListRow from "@/components/chat/ThreadsListRow.vue";

const STATUS_OPTIONS: Array<{ value: ThreadsStatusFilter; label: string }> = [
  { value: "all", label: "All" },
  { value: "unread", label: "Unread" },
  { value: "following", label: "Following" },
];

const SORT_OPTIONS: Array<{ value: ThreadsSort; label: string }> = [
  { value: "recent", label: "Recently active" },
  { value: "unread", label: "Most unread" },
  { value: "replies", label: "Most replies" },
];

const props = defineProps<{
  xmppClient: BrowserXmppClient | null;
  channels: readonly ChannelSummary[];
  /** Resolves a thread starter's avatar JID from loaded rows; unknown → initials. */
  rootAuthorJid?: (entry: WasmThreadEntry) => string | null;
}>();

const emit = defineEmits<{
  openThread: [entry: WasmThreadEntry];
  joinCall: [entry: WasmThreadEntry, media: CallMedia];
}>();

const loadMoreClass = cx(btn({ size: "sm" }), css({ alignSelf: "flex-start" }));
const retryClass = btn({ size: "sm" });
const loaderClass = waddle();

// Disc-List board: search, the All / Unread / Following switch and a sort.
// The time-window and channel filters are cut to keep the page calm, so the
// default window is all time; a `?active=` / `?channel=` URL still applies.
const rootClass = css({ display: "flex", flexDirection: "column", gap: "2.5", md: { gap: "6" } });
const filtersClass = css({ display: "flex", flexWrap: "wrap", alignItems: "center", gap: "2" });
const searchClass = css({
  flex: "1 1 240px",
  display: "flex",
  alignItems: "center",
  gap: "2",
  height: "44px",
  md: { height: "40px" },
  boxSizing: "border-box",
  px: "3",
  borderRadius: "control",
  bg: "surface",
  border: "1px solid {colors.border.strong}",
  color: "fg.muted",
  _focusWithin: { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
  "& svg": { width: "16px", height: "16px", flex: "none" },
  "& input": { flex: 1, minWidth: 0, border: 0, bg: "transparent", color: "fg", font: "14px {fonts.body}", outline: "none" },
  "& input::placeholder": { color: "fg.soft" },
});
const segClass = cx(seg(), css({ _coarse: { "& > *": { height: "38px" } } }));
const sortClass = cx(input(), css({ height: "44px", minWidth: 0, md: { height: "40px", minHeight: "40px" } }));
const summaryClass = css({ margin: 0, textStyle: "caption", color: "fg.muted" });
const sectionClass = css({ display: "flex", flexDirection: "column", gap: "2" });
const listClass = css({
  margin: 0,
  padding: 0,
  listStyle: "none",
  borderRadius: "panel",
  border: "1px solid {colors.border.soft}",
  bg: "surface",
  overflow: "hidden",
  "& > li + li": { borderTop: "1px solid {colors.border.soft}" },
});
const emptyClass = cx(pc.empty, css({ pt: "48px" }));
const inlineErrorClass = css({ margin: 0, textStyle: "caption", color: "danger.text" });
const srOnly = css({ srOnly: true });
const grow = css({ flex: 1, minWidth: 0 });

const browserUrlState = typeof window === "undefined" ? undefined : {
  readSearch: () => window.location.search,
  replaceSearch: (encoded: string) => {
    const next = encoded
      ? `${window.location.pathname}?${encoded}${window.location.hash}`
      : `${window.location.pathname}${window.location.hash}`;
    window.history.replaceState(window.history.state, "", next);
  },
};

const state = useThreadsListPanelState({
  xmppClient: computed(() => props.xmppClient),
  channels: computed(() => props.channels),
  urlState: browserUrlState,
});

const {
  channelFilterMessage,
  error,
  fetchThreadsPage,
  hasEntries,
  loading,
  loadingMore,
  markThreadRead,
  markingReadKey,
  nextCursor,
  resultSummary,
  search,
  sections,
  sort,
  status,
  threadKey,
} = state;

onMounted(() => {
  void fetchThreadsPage(false);
});
</script>

<template>
  <div :class="rootClass">
    <div :class="filtersClass">
      <label :class="searchClass">
        <Search aria-hidden="true" />
        <input
          v-model="search"
          type="search"
          placeholder="Search discussions"
          aria-label="Search discussions"
        />
      </label>
      <div :class="segClass" role="group" aria-label="Discussion status filter">
        <button
          v-for="option in STATUS_OPTIONS"
          :key="option.value"
          type="button"
          :aria-pressed="status === option.value ? 'true' : 'false'"
          @click="status = option.value"
        >
          {{ option.label }}
        </button>
      </div>
      <select
        v-model="sort"
        :class="sortClass"
        aria-label="Sort discussions"
      >
        <option
          v-for="option in SORT_OPTIONS"
          :key="option.value"
          :value="option.value"
        >
          {{ option.label }}
        </option>
      </select>
    </div>

    <p v-if="resultSummary" :class="summaryClass">{{ resultSummary }}</p>

    <div v-if="loading && !hasEntries" :class="pc.status" role="status" aria-busy="true">
      <span :class="loaderClass" aria-hidden="true"><i></i><i></i><i></i></span>
      Loading discussions…
    </div>

    <div v-else-if="error && !hasEntries" :class="pc.alert" role="alert">
      <span :class="grow">Couldn't load discussions: {{ error }}</span>
      <button type="button" :class="retryClass" @click="fetchThreadsPage(false)">Retry</button>
    </div>

    <template v-else>
      <section
        v-for="section in sections"
        v-show="section.entries.length > 0"
        :key="section.id"
        :class="sectionClass"
      >
        <h2 :class="srOnly">
          {{ section.label }} · {{ section.entries.length }}
        </h2>
        <ul :class="listClass">
          <li v-for="entry in section.entries" :key="threadKey(entry)">
            <ThreadsListRow
              :entry="entry"
              :marking-read="markingReadKey === threadKey(entry)"
              :root-author-jid="rootAuthorJid?.(entry) ?? null"
              @open="emit('openThread', $event)"
              @mark-read="markThreadRead"
              @join-call="(joined, media) => emit('joinCall', joined, media)"
            />
          </li>
        </ul>
      </section>

      <div v-if="!hasEntries" :class="emptyClass" role="status">
        <img :src="spotEmpty" alt="" />
        <h2 :class="pc.emptyTitle">{{ search ? "Nothing matches" : "It is quiet here" }}</h2>
        <p>
          {{ channelFilterMessage || (search ? "Try another search, or ask a room and start a discussion." : "Ask a room something and start a discussion.") }}
        </p>
      </div>

      <p v-if="error && hasEntries" :class="inlineErrorClass" role="alert">
        Couldn't refresh discussions: {{ error }}
      </p>

      <button
        v-if="nextCursor"
        type="button"
        :class="loadMoreClass"
        :disabled="loadingMore"
        @click="fetchThreadsPage(true)"
      >
        {{ loadingMore ? "Loading…" : "Load more" }}
      </button>

      <div v-if="markingReadKey" :class="srOnly" aria-live="polite">
        Marking discussion read
      </div>
    </template>
  </div>
</template>
