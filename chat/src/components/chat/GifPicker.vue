<script setup lang="ts">
import { ref, watch } from "vue";
import { Search, X } from "lucide-vue-next";

const props = defineProps<{
  apiKey: string;
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  select: [url: string];
  close: [];
}>();

interface GiphyGif {
  id: string;
  title: string;
  images: {
    fixed_height_small: { url: string; width: string; height: string };
    original: { url: string };
  };
}

const query = ref("");
const results = ref<GiphyGif[]>([]);
const isLoading = ref(false);
let debounceTimer: ReturnType<typeof setTimeout> | null = null;

async function fetchGifs(searchQuery: string) {
  if (!props.apiKey) return;
  isLoading.value = true;
  try {
    const endpoint = searchQuery.trim()
      ? `https://api.giphy.com/v1/gifs/search?q=${encodeURIComponent(searchQuery)}&api_key=${props.apiKey}&limit=24&rating=g`
      : `https://api.giphy.com/v1/gifs/trending?api_key=${props.apiKey}&limit=24&rating=g`;
    const res = await fetch(endpoint);
    if (res.ok) {
      const data = await res.json();
      results.value = data.data ?? [];
    }
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
    class="absolute left-0 right-0 glass-panel border border-border rounded-xl max-h-96 flex flex-col z-50 shadow-2xl animate-fade-in overflow-hidden"
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
          v-model="query"
          type="text"
          placeholder="Search GIFs..."
          aria-label="Search GIFs"
          class="min-h-10 w-full rounded-lg border-none bg-transparent py-2.5 pl-9 pr-11 text-[13px] leading-5 placeholder:text-muted-foreground/40 focus:outline-none"
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

    <!-- No API key -->
    <div v-if="!apiKey" class="flex h-24 items-center justify-center px-4 text-center text-[13px] text-muted-foreground">
      GIPHY_API_KEY not configured.
    </div>

    <!-- Results -->
    <div v-else class="flex-1 overflow-auto p-2">
      <div v-if="isLoading" class="flex h-24 items-center justify-center text-[13px] text-muted-foreground">
        <div class="flex items-center justify-center gap-1.5">
          <span class="typing-dot" />
          <span class="typing-dot" />
          <span class="typing-dot" />
        </div>
      </div>
      <div v-else-if="results.length === 0" class="flex h-24 items-center justify-center rounded-lg text-center text-[13px] text-muted-foreground">
        {{ query ? "No GIFs found" : "Loading trending GIFs..." }}
      </div>
      <div v-else class="grid grid-cols-3 gap-2">
        <button
          v-for="gif in results"
          :key="gif.id"
          type="button"
          class="aspect-square overflow-hidden rounded-lg bg-muted/40 ring-1 ring-border/50 transition-all duration-200 hover:ring-primary/35 hover:shadow-[0_0_8px_var(--glow)] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary/35"
          @click="selectGif(gif)"
        >
          <img
            :src="gif.images.fixed_height_small.url"
            :alt="gif.title"
            class="h-full w-full object-cover"
            loading="lazy"
          />
        </button>
      </div>
    </div>

    <!-- Attribution -->
    <div class="flex h-8 items-center justify-end border-t border-border px-3">
      <span class="text-[10px] font-medium text-muted-foreground/50">Powered by GIPHY</span>
    </div>
  </div>
</template>
