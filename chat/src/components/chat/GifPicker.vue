<script setup lang="ts">
import { onMounted, ref, watch } from "vue";
import { css } from "styled-system/css";
import { iconbtn } from "styled-system/recipes";
import { Loader2, Search, SearchX, X } from "lucide-vue-next";
import {
  GIF_SEARCH_UNAVAILABLE_MESSAGE,
  resolveGifPickerResponse,
  type GifPickerFetchState,
  type GiphyGif,
} from "@/lib/gif-picker-fetch";
import { composerPopover, pickerSearch, spin } from "./composer-classes";

const props = defineProps<{
  isTopPinned?: boolean;
  /** Search to run on open, e.g. from `/giphy cats`. */
  initialQuery?: string;
}>();

const emit = defineEmits<{
  select: [url: string];
  close: [];
}>();

const searchInput = ref<HTMLInputElement | null>(null);
onMounted(() => {
  searchInput.value?.focus();
});

const query = ref(props.initialQuery ?? "");
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

fetchGifs(query.value);

function selectGif(gif: GiphyGif) {
  emit("select", gif.images.original.url);
}

const gifRoot = css({ maxHeight: "384px", p: 0, gap: 0, overflow: "hidden" });
const results_ = css({ flex: 1, overflow: "auto", p: "2" });
const row = css({ flexDirection: "row" });
const smallIcon = css({ width: "14px!", height: "14px!" });
const grid = css({ display: "grid", gridTemplateColumns: "repeat(3, minmax(0, 1fr))", gap: "2" });
const tile = css({
  aspectRatio: "1",
  overflow: "hidden",
  p: 0,
  border: "1px solid {colors.border.soft}",
  borderRadius: "control",
  bg: "surface.2",
  cursor: "pointer",
  transition: "border-color {durations.fast} {easings.out}",
  _hover: { borderColor: "action.text" },
  "& img": { width: "100%", height: "100%", objectFit: "cover" },
});
const attribution = css({ display: "flex", alignItems: "center", justifyContent: "flex-end", height: "32px", px: "3", borderTop: "1px solid {colors.border.soft}", textStyle: "caption", fontWeight: 700, color: "fg.soft" });
</script>

<template>
  <div :class="[composerPopover.root, gifRoot, isTopPinned ? composerPopover.below : composerPopover.above]">
    <!-- Header -->
    <div :class="pickerSearch.bar">
      <div :class="pickerSearch.box">
        <Search :class="pickerSearch.icon" aria-hidden="true" />
        <input
          ref="searchInput"
          v-model="query"
          type="text"
          placeholder="Search GIFs…"
          aria-label="Search GIFs"
          :class="pickerSearch.input"
        />
        <button
          type="button"
          :class="[iconbtn({ size: 'sm' }), pickerSearch.close]"
          aria-label="Close GIF picker"
          @click="emit('close')"
        >
          <X aria-hidden="true" />
        </button>
      </div>
    </div>

    <!-- Server has no Giphy key configured (proxy answered 503) -->
    <div v-if="notConfigured" :class="pickerSearch.state">
      GIF search is not configured.
    </div>

    <!-- Transient proxy failure (429 rate limit / 502 upstream) -->
    <div v-else-if="errorMessage" :class="pickerSearch.state">
      {{ errorMessage }}
    </div>

    <!-- Results -->
    <div v-else :class="results_">
      <div v-if="isLoading" :class="[pickerSearch.state, row]">
        <Loader2 :class="[smallIcon, spin]" aria-hidden="true" />
        <span>Loading…</span>
      </div>
      <div
        v-else-if="results.length === 0 && query.trim().length > 0"
        :class="pickerSearch.state"
      >
        <SearchX aria-hidden="true" />
        <span>No GIFs found</span>
      </div>
      <!-- Idle state — when no query is active but the trending fetch
           didn't return anything. Rare; keep as a plain caption. -->
      <div
        v-else-if="results.length === 0"
        :class="pickerSearch.state"
      >
        Loading trending GIFs…
      </div>
      <div v-else :class="grid">
        <button
          v-for="gif in results"
          :key="gif.id"
          type="button"
          :class="tile"
          :aria-label="`Select ${gif.title || 'GIF'}`"
          @click="selectGif(gif)"
        >
          <img
            :src="gif.images.fixed_height_small.url"
            :alt="gif.title || 'GIF result'"
            loading="lazy"
          />
        </button>
      </div>
    </div>

    <!-- Attribution -->
    <div :class="attribution">
      <span>Powered by GIPHY</span>
    </div>
  </div>
</template>
