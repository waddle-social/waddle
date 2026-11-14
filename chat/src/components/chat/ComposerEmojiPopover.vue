<script setup lang="ts">
import { composerPopover } from "./composer-classes";
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
    :class="[composerPopover.root, isTopPinned ? composerPopover.below : composerPopover.above]"
  >
      <button
        v-for="(entry, i) in results"
        :key="entry.name"
        type="button"
        :class="composerPopover.option"
        :data-selected="i === selectedIndex || undefined"
        @mousedown.prevent="emit('pick', entry.emoji)"
      >
        <span>{{ entry.emoji }}</span>
        <span :class="composerPopover.hint">:{{ entry.name }}:</span>
      </button>
  </div>
</template>
