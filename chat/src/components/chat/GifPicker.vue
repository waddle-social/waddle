<script setup lang="ts">
import { ref, watch } from "vue";
import { Search, X } from "lucide-vue-next";

const props = defineProps<{
  apiKey: string;
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

// Load trending on mount
fetchGifs("");

function selectGif(gif: GiphyGif) {
  emit("select", gif.images.original.url);
}
</script>

<template>
  <div class="absolute bottom-full left-0 right-0 mb-1 bg-background border border-foreground max-h-80 flex flex-col z-50">
    <!-- Header -->
    <div class="flex items-center gap-2 px-3 py-2 border-b border-foreground">
      <Search class="w-3.5 h-3.5 text-muted-foreground flex-shrink-0" />
      <input
        v-model="query"
        placeholder="Search GIFs..."
        class="flex-1 font-mono text-sm bg-transparent border-none focus:outline-none"
      />
      <button
        class="text-muted-foreground hover:text-foreground flex-shrink-0"
        @click="emit('close')"
      >
        <X class="w-3.5 h-3.5" />
      </button>
    </div>

    <!-- No API key warning -->
    <div v-if="!apiKey" class="p-4 text-center text-sm font-mono text-muted-foreground">
      GIPHY_API_KEY not configured. Add it to your environment variables.
    </div>

    <!-- Results grid -->
    <div v-else class="flex-1 overflow-auto p-2">
      <div v-if="isLoading" class="text-center py-4 text-sm font-mono text-muted-foreground">
        Loading...
      </div>
      <div v-else-if="results.length === 0" class="text-center py-4 text-sm font-mono text-muted-foreground">
        {{ query ? "No GIFs found" : "Loading trending GIFs..." }}
      </div>
      <div v-else class="grid grid-cols-3 gap-1">
        <button
          v-for="gif in results"
          :key="gif.id"
          class="aspect-square overflow-hidden border border-foreground/20 hover:border-foreground transition-colors"
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

    <!-- Giphy attribution -->
    <div class="px-3 py-1 border-t border-foreground/30 text-right">
      <span class="text-[10px] font-mono text-muted-foreground">Powered by GIPHY</span>
    </div>
  </div>
</template>
