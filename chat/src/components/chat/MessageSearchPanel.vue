<script setup lang="ts">
import { nextTick, ref, watch } from "vue";
import { Loader2, Search, SearchX, X } from "lucide-vue-next";
import AppAvatar from "@/components/ui/AppAvatar.vue";
import { formatTimelineStamp } from "@/channels/timeline";
import type { MessageSearchResult, OccupantPresence } from "@/lib/xmpp-client";

const open = defineModel<boolean>("open", { default: false });

defineProps<{
  results: MessageSearchResult[];
  isSearching: boolean;
  avatarUrlByAuthor: Record<string, string | null>;
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
</script>

<template>
  <!-- Search bar -->
  <div v-if="open" class="px-[var(--chat-content-inline)] py-2.5 border-b border-border glass-surface flex items-center gap-3 flex-shrink-0 animate-fade-in">
    <div class="chat-message-lane flex items-center gap-3">
      <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
      <input
        ref="searchInputEl"
        v-model="searchInput"
        placeholder="Search messages…"
        aria-label="Search messages"
        class="type-field flex-1 bg-transparent focus:outline-none placeholder:text-muted-foreground/40"
        @keydown.enter="doSearch"
        @keydown.escape="closeSearch"
      />
      <button
        v-if="searchInput"
        class="chat-icon-button chat-icon-button--md text-muted-foreground hover:bg-muted hover:text-foreground"
        aria-label="Clear search"
        type="button"
        @click="closeSearch"
      >
        <X class="w-3.5 h-3.5" />
      </button>
    </div>
  </div>

  <!-- Search results -->
  <div v-if="open && (searchSubmitted || isSearching)" class="border-b border-border glass-surface max-h-56 overflow-auto flex-shrink-0">
    <div class="chat-message-lane">
      <!-- Loading state — Loader2 spinner alongside the copy so the
           user sees the request is in flight, not just frozen. -->
      <div
        v-if="isSearching"
        class="type-caption flex items-center justify-center gap-2 px-[var(--chat-content-inline)] py-5 text-muted-foreground"
      >
        <Loader2 class="h-3.5 w-3.5 motion-safe:animate-spin" aria-hidden="true" />
        <span>Searching…</span>
      </div>
      <!-- Empty state — SearchX glyph stacked above the copy so the
           "no matches" outcome reads as an authored state, not a
           one-line ghost. -->
      <div
        v-else-if="results.length === 0"
        class="flex flex-col items-center justify-center gap-1.5 px-[var(--chat-content-inline)] py-6 text-center"
      >
        <SearchX class="h-5 w-5 text-muted-foreground/60" aria-hidden="true" />
        <span class="type-caption text-muted-foreground">No matching messages</span>
      </div>
      <div v-else class="divide-y divide-border">
        <!-- Each search result row gets the author's avatar on the
             left so the eye can triage results by who-said-it at a
             glance — the same recognition cue the timeline uses.
             Avatar (xs) + content stack (name + time on row one,
             truncated body on row two) lets the row read as a
             compact preview of the matching message. -->
        <button
          v-for="result in results"
          :key="result.id"
          class="flex w-full items-start gap-3 px-[var(--chat-content-inline)] py-3 text-left hover:bg-muted/50 transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/25"
          type="button"
          @click="emit('openResult', result)"
        >
          <AppAvatar
            :name="result.nick"
            :src="avatarUrlByAuthor[result.nick] ?? null"
            :presence="roomPresence[result.nick] ?? 'offline'"
            size="xs"
          />
          <div class="min-w-0 flex-1">
            <div class="flex items-baseline gap-2">
              <span class="type-control">{{ result.nick }}</span>
              <span class="type-meta type-numeric text-muted-foreground">{{ formatTimelineStamp(result.createdAt) }}</span>
            </div>
            <p class="type-caption truncate text-muted-foreground">{{ result.body }}</p>
          </div>
        </button>
      </div>
    </div>
  </div>
</template>
