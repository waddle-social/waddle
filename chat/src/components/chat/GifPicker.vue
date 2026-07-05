<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { Loader2, Search, SearchX, X } from "lucide-vue-next";
import {
  GIF_SEARCH_UNAVAILABLE_MESSAGE,
  resolveGifPickerResponse,
  type GifPickerFetchState,
  type GiphyGif,
} from "@/lib/gif-picker-fetch";

defineProps<{
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  select: [url: string];
  close: [];
}>();

const searchInput = ref<HTMLInputElement | null>(null);
onMounted(() => {
  searchInput.value?.focus();
});

const query = ref("");
const results = ref<GiphyGif[]>([]);
const isLoading = ref(false);
const notConfigured = ref(false);
const errorMessage = ref<string | null>(null);
let debounceTimer: ReturnType<typeof setTimeout> | null = null;

// Searches go through the same-origin `/api/giphy` proxy so the Giphy
// key never reaches the browser (see `@/lib/giphy-proxy`). Rating and
// key handling live server-side; the client only sends `q` + `limit`.
// Response classification (503 not-configured vs 429/502 error vs ok)
// lives in `@/lib/gif-picker-fetch` so the latest response always
// drives the panel and failed fetches never keep stale results.
async function fetchGifs(searchQuery: string) {
  isLoading.value = true;
  try {
    const params = new URLSearchParams({ limit: "24" });
    const q = searchQuery.trim();
    if (q) params.set("q", q);
    const state = await fetch(`/api/giphy?${params}`).then(
      resolveGifPickerResponse,
      (): GifPickerFetchState => ({
        notConfigured: false,
        results: [],
        errorMessage: GIF_SEARCH_UNAVAILABLE_MESSAGE,
      }),
    );
    notConfigured.value = state.notConfigured;
    results.value = state.results;
    errorMessage.value = state.errorMessage;
  } finally {
    isLoading.value = false;
  }
}

watch(query, (q) => {
  if (debounceTimer) clearTimeout(debounceTimer);
  debounceTimer = setTimeout(() => fetchGifs(q), 300);
});

fetchGifs("");

function selectGif(gif: GiphyGif) {
  emit("select", gif.images.original.url);
}
</script>

<template>
  <div
    class="chat-composer-popover z-popover absolute glass-panel border border-border rounded-lg max-h-96 flex flex-col shadow-2xl animate-fade-in overflow-hidden"
    :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
  >
    <!-- Header -->
    <div class="border-b border-border p-2">
      <div class="relative min-w-0 rounded-lg bg-muted/60 transition-colors focus-within:bg-muted/75 focus-within:ring-1 focus-within:ring-inset focus-within:ring-primary/20">
        <Search
          class="pointer-events-none absolute left-3 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground"
          aria-hidden="true"
        />
        <input
          ref="searchInput"
          v-model="query"
          type="text"
          placeholder="Search GIFs…"
          aria-label="Search GIFs"
          class="type-field min-h-10 w-full rounded-lg border-none bg-transparent py-2.5 pl-9 pr-11 placeholder:text-muted-foreground/40 focus:outline-none"
        />
        <button
          type="button"
          class="absolute right-1.5 top-1/2 flex h-7 w-7 -translate-y-1/2 items-center justify-center rounded-md text-muted-foreground transition-colors hover:bg-muted hover:text-foreground"
          aria-label="Close GIF picker"
          @click="emit('close')"
        >
          <X class="w-3.5 h-3.5" aria-hidden="true" />
        </button>
      </div>
    </div>

    <!-- Server has no Giphy key configured (proxy answered 503) -->
    <div v-if="notConfigured" class="type-control flex h-24 items-center justify-center px-4 text-center text-muted-foreground">
      GIF search is not configured.
    </div>

    <!-- Transient proxy failure (429 rate limit / 502 upstream) -->
    <div v-else-if="errorMessage" class="type-control flex h-24 items-center justify-center px-4 text-center text-muted-foreground">
      {{ errorMessage }}
    </div>

    <!-- Results -->
    <div v-else class="flex-1 overflow-auto p-2">
      <!-- Loading state — Loader2 spinner matches the iter-47 channel
           search loading treatment so every "fetching" surface in the
           app spins with the same affordance. -->
      <div v-if="isLoading" class="type-caption flex h-24 items-center justify-center gap-2 text-muted-foreground">
        <Loader2 class="h-3.5 w-3.5 motion-safe:animate-spin" aria-hidden="true" />
        <span>Loading…</span>
      </div>
      <!-- Empty search state — SearchX glyph + caption matches the
           iter-48 EmojiPicker / iter-47 channel-search empty state. -->
      <div
        v-else-if="results.length === 0 && query.trim().length > 0"
        class="flex h-24 flex-col items-center justify-center gap-1.5 rounded-lg text-center"
      >
        <SearchX class="h-5 w-5 text-muted-foreground/60" aria-hidden="true" />
        <span class="type-caption text-muted-foreground">No GIFs found</span>
      </div>
      <!-- Idle state — when no query is active but the trending fetch
           didn't return anything. Rare; keep as a plain caption. -->
      <div
        v-else-if="results.length === 0"
        class="type-caption flex h-24 items-center justify-center rounded-lg text-center text-muted-foreground"
      >
        Loading trending GIFs…
      </div>
      <div v-else class="grid grid-cols-3 gap-2">
        <button
          v-for="gif in results"
          :key="gif.id"
          type="button"
          class="aspect-square overflow-hidden rounded-lg bg-muted/40 ring-1 ring-border/50 transition-all duration-200 hover:ring-primary/35 hover:shadow-[0_0_8px_var(--glow)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/35"
          :aria-label="`Select ${gif.title || 'GIF'}`"
          @click="selectGif(gif)"
        >
          <img
            :src="gif.images.fixed_height_small.url"
            :alt="gif.title || 'GIF result'"
            class="h-full w-full object-cover"
            loading="lazy"
          />
        </button>
      </div>
    </div>

    <!-- Attribution -->
    <div class="flex h-8 items-center justify-end border-t border-border px-3">
      <span class="type-meta type-emphasis text-muted-foreground/50">Powered by GIPHY</span>
    </div>
  </div>
</template>
