<script setup lang="ts">
interface EmojiResult {
  name: string;
  emoji: string;
}

defineProps<{
  results: EmojiResult[];
  selectedIndex: number;
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  pick: [emoji: string];
}>();
</script>

<template>
  <!-- :emoji autocomplete -->
  <div
    class="z-popover chat-composer-popover absolute glass-panel border border-border rounded-lg max-h-56 overflow-auto min-w-0 shadow-xl animate-fade-in p-1"
    :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
  >
    <div class="flex flex-col gap-1">
      <button
        v-for="(entry, i) in results"
        :key="entry.name"
        type="button"
        class="type-control w-full h-9 px-3 py-0 text-left transition-colors flex items-center gap-2 rounded-lg"
        :class="i === selectedIndex
          ? 'bg-primary/15 hover:bg-primary/20'
          : 'hover:bg-muted'"
        @mousedown.prevent="emit('pick', entry.emoji)"
      >
        <span>{{ entry.emoji }}</span>
        <span class="type-caption text-muted-foreground">:{{ entry.name }}:</span>
      </button>
    </div>
  </div>
</template>
