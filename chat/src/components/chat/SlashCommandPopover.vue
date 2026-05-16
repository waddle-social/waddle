<script setup lang="ts">
import { Slash, AlertCircle } from "lucide-vue-next";
import type { DiscoveredExtensionCommand } from "@/lib/xmpp/extension-commands";

defineProps<{
  candidates: DiscoveredExtensionCommand[];
  selectedIndex: number;
  prefix: string;
  blocked: boolean;
  isTopPinned?: boolean;
}>();

const emit = defineEmits<{
  pick: [command: DiscoveredExtensionCommand];
}>();
</script>

<template>
  <div
    class="z-popover chat-composer-popover absolute glass-panel border border-border rounded-lg max-h-56 overflow-auto min-w-0 shadow-xl animate-fade-in p-1"
    :class="isTopPinned ? 'top-full mt-2' : 'bottom-full mb-2'"
  >
    <div v-if="blocked" class="type-caption flex items-center gap-2 px-3 py-2 text-destructive">
      <AlertCircle class="h-3.5 w-3.5" aria-hidden="true" />
      <span>No command <span class="type-emphasis">/{{ prefix }}</span>. Press Esc, then Enter to send as text.</span>
    </div>
    <div v-else class="flex flex-col gap-1">
      <button
        v-for="(command, i) in candidates"
        :key="command.node"
        type="button"
        class="type-control w-full h-9 px-3 py-0 text-left transition-colors flex items-center gap-2 rounded-lg"
        :class="i === selectedIndex
          ? 'bg-primary/15 hover:bg-primary/20'
          : 'hover:bg-muted'"
        @mousedown.prevent="emit('pick', command)"
      >
        <Slash class="h-4 w-4 text-primary" aria-hidden="true" />
        <span class="type-emphasis">/{{ command.composerPrefix }}</span>
        <span class="type-caption text-muted-foreground truncate">{{ command.name }}</span>
      </button>
    </div>
  </div>
</template>
