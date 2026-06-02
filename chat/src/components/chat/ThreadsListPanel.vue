<script setup lang="ts">
import { computed, onMounted } from "vue";
import type { BrowserXmppClient } from "@/lib/xmpp-client";
import type { ChannelSummary } from "@/lib/chat-types";
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
  { value: "all", label: "All" },
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
}>();

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
  <div class="chat-panel-stack p-4">
    <div class="flex flex-col gap-3 border-b border-border/70 pb-3">
      <div class="flex flex-wrap items-center justify-between gap-2">
        <div>
          <h2 class="type-pane-title">Threads</h2>
          <div v-if="resultSummary" class="type-caption text-muted-foreground">
            {{ resultSummary }}
          </div>
        </div>
        <select
          v-model="sort"
          class="type-caption h-8 rounded-md border border-border bg-background px-2 text-foreground"
          aria-label="Sort threads"
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

      <div class="flex flex-wrap gap-2" aria-label="Thread status filter">
        <button
          v-for="option in STATUS_OPTIONS"
          :key="option.value"
          type="button"
          class="type-caption rounded-md border px-2.5 py-1"
          :class="status === option.value ? 'border-primary bg-primary text-primary-foreground' : 'border-border text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
          @click="status = option.value"
        >
          {{ option.label }}
        </button>
      </div>

      <div class="grid gap-2 md:grid-cols-[auto_minmax(10rem,14rem)_minmax(12rem,1fr)]">
        <div class="flex flex-wrap gap-1" aria-label="Thread activity window">
          <button
            v-for="option in ACTIVE_OPTIONS"
            :key="option.value"
            type="button"
            class="type-caption h-8 rounded-md border px-2"
            :class="active === option.value ? 'border-primary bg-primary/10 text-primary' : 'border-border text-muted-foreground hover:bg-muted/50 hover:text-foreground'"
            @click="active = option.value"
          >
            {{ option.label }}
          </button>
        </div>

        <select
          v-model="channel"
          class="type-caption h-8 min-w-0 rounded-md border border-border bg-background px-2 text-foreground"
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
          class="type-caption h-8 min-w-0 rounded-md border border-border bg-background px-2 text-foreground placeholder:text-muted-foreground"
          placeholder="Search threads"
          aria-label="Search threads"
        />
      </div>
    </div>

    <div v-if="loading && !hasEntries" class="type-caption text-muted-foreground" aria-busy="true">
      Loading threads…
    </div>

    <div v-else-if="error && !hasEntries" class="type-caption text-destructive">
      Couldn't load threads: {{ error }}
    </div>

    <template v-else>
      <section
        v-for="section in sections"
        v-show="section.entries.length > 0"
        :key="section.id"
        class="chat-panel-stack"
      >
        <div class="type-section-label text-muted-foreground/75">
          {{ section.label }} · {{ section.entries.length }}
        </div>
        <ThreadsListRow
          v-for="entry in section.entries"
          :key="threadKey(entry)"
          :entry="entry"
          :marking-read="markingReadKey === threadKey(entry)"
          @open="emit('openThread', $event)"
          @mark-read="markThreadRead"
        />
      </section>

      <div
        v-if="!hasEntries"
        class="type-caption text-muted-foreground"
      >
        {{ channelFilterMessage || "No threads match these filters." }}
      </div>

      <div v-if="error && hasEntries" class="type-caption text-destructive">
        Couldn't refresh threads: {{ error }}
      </div>

      <button
        v-if="nextCursor"
        type="button"
        class="type-control self-start rounded-md border border-border px-3 py-1.5 text-muted-foreground hover:bg-muted/50 hover:text-foreground disabled:opacity-60"
        :disabled="loadingMore"
        @click="fetchThreadsPage(true)"
      >
        {{ loadingMore ? "Loading…" : "Load more" }}
      </button>

      <div v-if="markingReadKey" class="sr-only" aria-live="polite">
        Marking thread read
      </div>
    </template>
  </div>
</template>
