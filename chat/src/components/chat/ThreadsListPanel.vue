<script setup lang="ts">
import { computed, onMounted } from "vue";
import { button, kicker } from "styled-system/recipes";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ChannelSummary } from "@/lib/chat-types";
import type { CallMedia } from "@/lib/calls/types";
import type { WasmThreadEntry } from "@/lib/xmpp/wasm-types";
import {
  type ThreadsActiveWindow,
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

const ACTIVE_OPTIONS: Array<{ value: ThreadsActiveWindow; label: string }> = [
  { value: "7d", label: "7d" },
  { value: "14d", label: "14d" },
  { value: "30d", label: "30d" },
  { value: "all", label: "All time" },
];

const SORT_OPTIONS: Array<{ value: ThreadsSort; label: string }> = [
  { value: "recent", label: "Recently active" },
  { value: "unread", label: "Most unread" },
  { value: "replies", label: "Most replies" },
];

const props = defineProps<{
  xmppClient: BrowserXmppClient | null;
  channels: readonly ChannelSummary[];
}>();

const emit = defineEmits<{
  openThread: [entry: WasmThreadEntry];
  joinCall: [entry: WasmThreadEntry, media: CallMedia];
}>();

const kickerClass = kicker();
const loadMoreClass = button({ variant: "quiet", size: "sm" });

// Filter pills: a plain hairline pill at rest, teal-filled when pressed.
const PILL_BASE = "inline-flex h-8 items-center rounded-full border px-3 text-[13px] font-semibold transition-colors";
const PILL_IDLE = "border-border bg-card text-muted-foreground hover:bg-muted hover:text-foreground";
const PILL_ON = "border-primary bg-primary text-primary-foreground";
const FIELD_CLASS = "h-8 min-w-0 rounded-full border border-border bg-card px-3 text-[13px] text-foreground placeholder:text-muted-foreground";

function pillClass(pressed: boolean): string {
  return `${PILL_BASE} ${pressed ? PILL_ON : PILL_IDLE}`;
}

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
  active,
  channel,
  channelFilterMessage,
  channelFilterOptionLabel,
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
  selectableChannels,
  sort,
  status,
  threadKey,
} = state;

onMounted(() => {
  void fetchThreadsPage(false);
});
</script>

<template>
  <div class="grid gap-4">
    <div class="grid gap-3">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <div class="flex flex-wrap gap-1.5" role="group" aria-label="Discussion status filter">
          <button
            v-for="option in STATUS_OPTIONS"
            :key="option.value"
            type="button"
            :class="pillClass(status === option.value)"
            :aria-pressed="status === option.value ? 'true' : 'false'"
            @click="status = option.value"
          >
            {{ option.label }}
          </button>
        </div>
        <label class="flex items-center gap-2">
          <span :class="kickerClass">Sort</span>
          <select
            v-model="sort"
            :class="FIELD_CLASS"
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
        </label>
      </div>

      <div class="grid gap-2 md:grid-cols-[auto_minmax(10rem,14rem)_minmax(12rem,1fr)]">
        <div class="flex flex-wrap gap-1.5" role="group" aria-label="Discussion activity window">
          <button
            v-for="option in ACTIVE_OPTIONS"
            :key="option.value"
            type="button"
            :class="pillClass(active === option.value)"
            :aria-pressed="active === option.value ? 'true' : 'false'"
            @click="active = option.value"
          >
            {{ option.label }}
          </button>
        </div>

        <select
          v-model="channel"
          :class="FIELD_CLASS"
          aria-label="Filter by channel"
        >
          <option value="all">All channels</option>
          <option
            v-if="channelFilterOptionLabel && channel !== 'all'"
            :value="channel"
          >
            {{ channelFilterOptionLabel }}
          </option>
          <option
            v-for="item in selectableChannels"
            :key="item.id"
            :value="item.id"
          >
            #{{ item.name || item.id }}
          </option>
        </select>

        <input
          v-model="search"
          type="search"
          :class="FIELD_CLASS"
          placeholder="Search discussions"
          aria-label="Search discussions"
        />
      </div>

      <p v-if="resultSummary" :class="kickerClass">{{ resultSummary }}</p>
    </div>

    <div v-if="loading && !hasEntries" class="type-caption text-muted-foreground" aria-busy="true">
      Loading discussions…
    </div>

    <div v-else-if="error && !hasEntries" class="type-caption text-destructive">
      Couldn't load discussions: {{ error }}
    </div>

    <template v-else>
      <section
        v-for="section in sections"
        v-show="section.entries.length > 0"
        :key="section.id"
        class="grid gap-2"
      >
        <h2 :class="kickerClass">
          {{ section.label }} · {{ section.entries.length }}
        </h2>
        <ThreadsListRow
          v-for="entry in section.entries"
          :key="threadKey(entry)"
          :entry="entry"
          :marking-read="markingReadKey === threadKey(entry)"
          @open="emit('openThread', $event)"
          @mark-read="markThreadRead"
          @join-call="(joined, media) => emit('joinCall', joined, media)"
        />
      </section>

      <div
        v-if="!hasEntries"
        class="rounded-xl border border-dashed border-border px-4 py-8 text-center"
      >
        <p class="font-display text-lg font-semibold text-foreground">It is quiet here.</p>
        <p class="type-caption mt-1 text-muted-foreground">
          {{ channelFilterMessage || "No discussions match these filters. Ask the room something and start one." }}
        </p>
      </div>

      <div v-if="error && hasEntries" class="type-caption text-destructive">
        Couldn't refresh discussions: {{ error }}
      </div>

      <button
        v-if="nextCursor"
        type="button"
        :class="[loadMoreClass, 'justify-self-start']"
        :disabled="loadingMore"
        @click="fetchThreadsPage(true)"
      >
        {{ loadingMore ? "Loading…" : "Load more" }}
      </button>

      <div v-if="markingReadKey" class="sr-only" aria-live="polite">
        Marking discussion read
      </div>
    </template>
  </div>
</template>
