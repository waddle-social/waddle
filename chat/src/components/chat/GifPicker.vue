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
    class="absolute left-0 right-0 glass-panel border border-border rounded-xl max-h-72 flex flex-col z-50 shadow-2xl animate-fade-in overflow-hidden"
    :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
  >
    <!-- Header -->
    <div class="flex items-center gap-2.5 px-4 py-2.5 border-b border-border">
      <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
      <input
        v-model="query"
        placeholder="Search GIFs..."
        class="flex-1 text-[13px] bg-transparent border-none focus:outline-none placeholder:text-muted-foreground/40"
      />
      <button
        class="p-1 rounded-lg text-muted-foreground hover:text-foreground transition-all duration-200 flex-shrink-0"
        @click="emit('close')"
      >
        <X class="w-3.5 h-3.5" />
      </button>
    </div>

    <!-- No API key -->
    <div v-if="!apiKey" class="p-5 text-center text-[13px] text-muted-foreground">
      GIPHY_API_KEY not configured.
    </div>

    <!-- Results -->
    <div v-else class="flex-1 overflow-auto p-2">
      <div v-if="isLoading" class="text-center py-5 text-[13px] text-muted-foreground">
        <div class="flex items-center justify-center gap-1.5">
          <span class="typing-dot" />
          <span class="typing-dot" />
          <span class="typing-dot" />
        </div>
      </div>
      <div v-else-if="results.length === 0" class="text-center py-5 text-[13px] text-muted-foreground">
        {{ query ? "No GIFs found" : "Loading trending GIFs..." }}
      </div>
      <div v-else class="grid grid-cols-3 gap-1.5">
        <button
          v-for="gif in results"
          :key="gif.id"
          class="aspect-square overflow-hidden rounded-lg hover:ring-2 hover:ring-primary/40 hover:shadow-[0_0_8px_var(--glow)] transition-all duration-200"
          @click="selectGif(gif)"
        >
          <img
            :src="gif.images.fixed_height_small.url"
            :alt="gif.title"
            class="w-full h-full object-cover"
            loading="lazy"
          />
        </button>
      </div>
    </div>

    <!-- Attribution -->
    <div class="px-4 py-1.5 border-t border-border text-right">
      <span class="text-[10px] text-muted-foreground/50">Powered by GIPHY</span>
    </div>
  </div>
</template>
