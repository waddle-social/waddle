<script setup lang="ts">
import type { ChannelSummary, WaddleSummary } from "@/lib/waddle-api";
import type { WaddleSession } from "@/lib/server-auth";

defineProps<{
  waddle: WaddleSummary | null;
  channel: ChannelSummary | null;
  session: WaddleSession | null;
}>();

const emit = defineEmits<{
  openNav: [];
  openDetails: [];
}>();
</script>

<template>
  <div class="lg:hidden sticky top-0 z-10 grid grid-cols-[auto_1fr_auto] gap-3 items-center p-3 border-b border-foreground bg-background">
    <button
      class="text-sm font-mono uppercase tracking-wider px-2 py-1 border border-foreground hover:bg-foreground hover:text-background transition-colors"
      @click="emit('openNav')"
    >
      Menu
    </button>
    <div class="min-w-0 text-center">
      <div class="font-mono font-bold text-sm truncate">
        {{ channel ? `#${channel.name}` : waddle?.name ?? "Waddle Chat" }}
      </div>
      <div class="text-xs font-mono text-muted-foreground truncate">
        {{ waddle?.name ?? session?.username }}
      </div>
    </div>
    <button
      class="text-sm font-mono uppercase tracking-wider px-2 py-1 border border-foreground hover:bg-foreground hover:text-background transition-colors"
      @click="emit('openDetails')"
    >
      Details
    </button>
  </div>
</template>
