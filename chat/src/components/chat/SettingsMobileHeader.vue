<script setup lang="ts">
import { Menu, Settings } from "lucide-vue-next";

const emit = defineEmits<{
  openNav: [];
}>();
</script>

<template>
  <div class="chat-mobile-header z-sticky lg:hidden sticky top-0 grid grid-cols-[auto_1fr_auto] items-center gap-3 border-b border-border px-3 py-1.5 glass-panel">
    <button
      class="h-11 w-11 flex items-center justify-center rounded-lg hover:bg-muted transition-all duration-200"
      type="button"
      aria-label="Open navigation"
      @click="emit('openNav')"
    >
      <Menu class="w-4 h-4" />
    </button>
    <div class="min-w-0 text-left">
      <div class="type-pane-title truncate flex items-center justify-start gap-1.5">
        <Settings class="w-3 h-3 text-primary/60" />
        Settings
      </div>
      <div class="type-caption text-muted-foreground truncate">
        Personal preferences
      </div>
    </div>
    <div class="h-11 w-11" aria-hidden="true" />
  </div>
</template>
